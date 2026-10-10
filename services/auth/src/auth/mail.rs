//! Sending email through Resend (https://resend.com): one HTTPS request per email,
//! so no mail server runs here.
//!
//! - `RESEND_API_KEY`: the API key. Without it nothing is sent; debug builds print
//!   each email (links included) to the log instead, so the flows can be tried
//!   locally. Release builds only log that an email was dropped: links are secrets.
//! - `MAIL_FROM`: the sender, e.g. `milesstorm.com <no-reply@milesstorm.com>`. Its
//!   domain must be verified in Resend.
//! - `SITE_URL`: the website's address, used in links (falls back to
//!   `BFF_CALLBACK_URL`, then `http://localhost:8080`).

use std::sync::Arc;

use serde::Serialize;

use super::trace::{self, Peer, TracedClient};

const RESEND_URL: &str = "https://api.resend.com/emails";
const DEFAULT_FROM: &str = "milesstorm.com <no-reply@milesstorm.com>";

/// One email, in plain text and HTML.
#[derive(Debug, Clone)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
}

#[derive(Clone)]
pub enum Mailer {
    Resend {
        http: TracedClient,
        /// `RESEND_URL`, or a stand-in server in tests.
        url: Arc<str>,
        key: Arc<str>,
        from: Arc<str>,
    },
    /// No API key: emails are logged (debug builds) or dropped.
    Off,
}

impl std::fmt::Debug for Mailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mailer::Resend { from, .. } => f.debug_struct("Resend").field("from", from).finish(),
            Mailer::Off => f.write_str("Off"),
        }
    }
}

impl Mailer {
    pub fn from_env() -> Self {
        match std::env::var("RESEND_API_KEY").ok().filter(|k| !k.trim().is_empty()) {
            Some(key) => {
                let from = std::env::var("MAIL_FROM")
                    .ok()
                    .filter(|f| !f.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_FROM.to_string());
                tracing::info!(%from, "sending email through Resend");
                Mailer::Resend {
                    http: resend_client(),
                    url: RESEND_URL.into(),
                    key: key.trim().into(),
                    from: from.into(),
                }
            }
            None if cfg!(debug_assertions) => {
                tracing::warn!("RESEND_API_KEY not set: emails are printed to this log instead of sent");
                Mailer::Off
            }
            None => {
                tracing::error!("RESEND_API_KEY not set: account emails (confirmations, password resets) can't be sent");
                Mailer::Off
            }
        }
    }

    /// Sends `mail`. `kind` names it in logs and metrics.
    #[tracing::instrument(name = "email.send", skip_all, fields(kind = kind))]
    pub async fn send(&self, kind: &str, mail: &Mail) -> Result<(), SendError> {
        let result = self.deliver(kind, mail).await;
        super::telemetry::email(kind, if result.is_ok() { "sent" } else { "failed" });
        // Resend's reply can repeat the address, so the span gets the error's type only.
        if let Err(e) = &result {
            trace::fail(&tracing::Span::current(), e.error_type.clone());
        }
        result
    }

    async fn deliver(&self, kind: &str, mail: &Mail) -> Result<(), SendError> {
        let (http, url, key, from) = match self {
            Mailer::Resend { http, url, key, from } => (http, url, key, from),
            Mailer::Off if cfg!(debug_assertions) => {
                tracing::info!(kind, to = %mail.to, subject = %mail.subject, body = %mail.text, "email (not sent: no RESEND_API_KEY)");
                return Ok(());
            }
            Mailer::Off => return Err(SendError::new("no_api_key", "no RESEND_API_KEY")),
        };

        #[derive(Serialize)]
        struct Body<'a> {
            from: &'a str,
            to: [&'a str; 1],
            subject: &'a str,
            text: &'a str,
            html: &'a str,
        }
        let resp = http
            .post(&**url)
            .bearer_auth(key)
            .header(reqwest::header::USER_AGENT, "milesstorm-auth")
            .json(&Body { from, to: [&mail.to], subject: &mail.subject, text: &mail.text, html: &mail.html })
            .send()
            .await
            .map_err(|e| SendError::new("unreachable", format!("Resend unreachable: {e}")))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        // Resend's error body says what's wrong (bad key, unverified domain, rate limit).
        let body = resp.text().await.unwrap_or_default();
        Err(SendError::new(
            status.as_str().to_owned(),
            format!("Resend returned {status}: {}", body.chars().take(300).collect::<String>()),
        ))
    }
}

/// The client for Resend's API.
fn resend_client() -> TracedClient {
    trace::client(
        Peer { service: "resend", expected: &[] },
        std::time::Duration::from_secs(15),
        reqwest::redirect::Policy::default(),
    )
}

/// Why an email wasn't sent. It prints as the full reason, for the log.
#[derive(Debug)]
pub struct SendError {
    /// Safe on a span: `unreachable`, `no_api_key`, or Resend's HTTP status.
    pub error_type: std::borrow::Cow<'static, str>,
    reason: String,
}

impl SendError {
    pub(super) fn new(error_type: impl Into<std::borrow::Cow<'static, str>>, reason: impl Into<String>) -> Self {
        Self { error_type: error_type.into(), reason: reason.into() }
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// The website's address, for links in emails.
pub fn site_url() -> String {
    std::env::var("SITE_URL")
        .or_else(|_| std::env::var("BFF_CALLBACK_URL"))
        .unwrap_or_else(|_| "http://localhost:8080".to_string())
        .trim_end_matches('/')
        .to_string()
}

/// An email with a greeting, some paragraphs, one button and a closing note, in the
/// website's default (dark) colours.
pub struct Letter<'a> {
    pub to: &'a str,
    pub subject: &'a str,
    /// Who it's for (display name or username).
    pub name: &'a str,
    pub paragraphs: &'a [&'a str],
    pub button: &'a str,
    pub link: &'a str,
    /// Under the button, smaller: why they got it and what to do if it wasn't them.
    pub footer: &'a str,
}

impl Letter<'_> {
    pub fn build(&self) -> Mail {
        let mut text = format!("Hi {},\n\n", self.name);
        for p in self.paragraphs {
            text.push_str(p);
            text.push_str("\n\n");
        }
        text.push_str(&format!("{}: {}\n\n{}\n\n— milesstorm.com\n", self.button, self.link, self.footer));

        let paragraphs: String = self
            .paragraphs
            .iter()
            .map(|p| format!(r#"<p style="margin:0 0 16px">{}</p>"#, escape(p)))
            .collect();
        let link = escape(self.link);
        let html = format!(
            r#"<!doctype html>
<html><head><meta name="color-scheme" content="dark"><meta name="supported-color-schemes" content="dark"></head><body style="margin:0;padding:0;background:#1d232a">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:#1d232a;padding:32px 16px">
<tr><td align="center">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:520px;background:#191e24;border-radius:12px;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;color:#d1d5db;font-size:16px;line-height:1.5">
<tr><td style="padding:32px 32px 8px;font-size:14px;font-weight:700;letter-spacing:.04em;color:#40c6ed">MILESSTORM.COM</td></tr>
<tr><td style="padding:8px 32px 0">
<p style="margin:0 0 16px">Hi {name},</p>
{paragraphs}
<p style="margin:24px 0"><a href="{link}" style="display:inline-block;background:#40c6ed;color:#191e24;text-decoration:none;font-weight:600;padding:12px 24px;border-radius:8px">{button}</a></p>
<p style="margin:0 0 16px;font-size:13px;color:#9ca3af">If the button doesn't work, copy this link into your browser:<br><a href="{link}" style="color:#40c6ed;word-break:break-all">{link}</a></p>
</td></tr>
<tr><td style="padding:16px 32px 32px;font-size:13px;color:#9ca3af;border-top:1px solid #2a323c">{footer}</td></tr>
</table>
</td></tr>
</table>
</body></html>"#,
            name = escape(self.name),
            button = escape(self.button),
            footer = escape(self.footer),
        );
        Mail { to: self.to.to_string(), subject: self.subject.to_string(), text, html }
    }
}

/// Escapes text for HTML (names are chosen by users).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::routing::post;
    use opentelemetry::trace::{SpanKind, Status};

    use super::super::trace::testing::{attr, pipeline, serve, text};
    use super::*;

    #[tokio::test]
    async fn sending_is_a_client_span_and_a_refusal_keeps_the_address_off_spans() {
        let traced = pipeline();
        let traceparent = Arc::new(Mutex::new(String::new()));
        let seen = traceparent.clone();
        let resend = serve(
            axum::Router::new()
                .route("/emails", post(move |headers: axum::http::HeaderMap| async move {
                    *seen.lock().unwrap() = headers["traceparent"].to_str().unwrap().to_string();
                }))
                // Resend's refusals repeat what was sent.
                .route("/refused", post(|body: String| async move {
                    (axum::http::StatusCode::UNPROCESSABLE_ENTITY, format!("invalid: {body}"))
                })),
        )
        .await;
        let mailer = |path: &str| Mailer::Resend {
            http: resend_client(),
            url: format!("{resend}{path}").into(),
            key: "key".into(),
            from: DEFAULT_FROM.into(),
        };
        let mail = Mail { to: "ada@example.com".into(), subject: "s".into(), text: "t".into(), html: "h".into() };

        mailer("/emails").send("verify_email", &mail).await.unwrap();
        let refused = mailer("/refused").send("verify_email", &mail).await.unwrap_err();
        // The log still gets Resend's reason.
        assert!(refused.to_string().contains("ada@example.com"));

        let [sent, failed] = &traced.named("email.send")[..] else { panic!("two email.send spans") };
        let client = traced.span("POST /emails");
        assert_eq!(client.span_kind, SpanKind::Client);
        assert_eq!(client.parent_span_id, sent.span_context.span_id());
        assert_eq!(attr(&client, "peer.service").as_deref(), Some("resend"));
        assert_eq!(
            *traceparent.lock().unwrap(),
            format!("00-{}-{}-01", sent.span_context.trace_id(), client.span_context.span_id())
        );
        assert_eq!(sent.status, Status::Unset);
        assert_eq!(failed.status, Status::error(""));
        assert_eq!(attr(failed, "error.type").as_deref(), Some("422"));
        for span in traced.spans() {
            assert!(!text(&span).contains("ada@example.com"), "{} holds the address", span.name);
        }
    }

    #[test]
    fn names_are_escaped_in_html_but_not_text() {
        let mail = Letter {
            to: "a@example.com",
            subject: "s",
            name: "<b>Eve</b>",
            paragraphs: &["one & two"],
            button: "Go",
            link: "https://milesstorm.com/x?code=a_b-c",
            footer: "f",
        }
        .build();
        assert!(mail.html.contains("Hi &lt;b&gt;Eve&lt;/b&gt;,"));
        assert!(!mail.html.contains("<b>Eve"));
        assert!(mail.html.contains("one &amp; two"));
        assert!(mail.text.starts_with("Hi <b>Eve</b>,"));
        assert!(mail.text.contains("Go: https://milesstorm.com/x?code=a_b-c"));
    }
}
