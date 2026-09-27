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
        http: reqwest::Client,
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
                    http: reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(15))
                        .build()
                        .expect("could not build the email HTTP client"),
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
    pub async fn send(&self, kind: &str, mail: &Mail) -> Result<(), String> {
        let result = self.deliver(kind, mail).await;
        super::telemetry::email(kind, if result.is_ok() { "sent" } else { "failed" });
        result
    }

    async fn deliver(&self, kind: &str, mail: &Mail) -> Result<(), String> {
        let (http, key, from) = match self {
            Mailer::Resend { http, key, from } => (http, key, from),
            Mailer::Off if cfg!(debug_assertions) => {
                tracing::info!(kind, to = %mail.to, subject = %mail.subject, body = %mail.text, "email (not sent: no RESEND_API_KEY)");
                return Ok(());
            }
            Mailer::Off => return Err("no RESEND_API_KEY".into()),
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
            .post(RESEND_URL)
            .bearer_auth(key)
            .header(reqwest::header::USER_AGENT, "milesstorm-auth")
            .json(&Body { from, to: [&mail.to], subject: &mail.subject, text: &mail.text, html: &mail.html })
            .send()
            .await
            .map_err(|e| format!("Resend unreachable: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        // Resend's error body says what's wrong (bad key, unverified domain, rate limit).
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Resend returned {status}: {}", body.chars().take(300).collect::<String>()))
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

/// An email with a greeting, some paragraphs, one button and a closing note.
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
<html><body style="margin:0;padding:0;background:#f4f4f5">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:#f4f4f5;padding:32px 16px">
<tr><td align="center">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:520px;background:#ffffff;border-radius:12px;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;color:#18181b;font-size:16px;line-height:1.5">
<tr><td style="padding:32px 32px 8px;font-size:14px;font-weight:700;letter-spacing:.04em;color:#6d28d9">MILESSTORM.COM</td></tr>
<tr><td style="padding:8px 32px 0">
<p style="margin:0 0 16px">Hi {name},</p>
{paragraphs}
<p style="margin:24px 0"><a href="{link}" style="display:inline-block;background:#6d28d9;color:#ffffff;text-decoration:none;font-weight:600;padding:12px 24px;border-radius:8px">{button}</a></p>
<p style="margin:0 0 16px;font-size:13px;color:#52525b">If the button doesn't work, copy this link into your browser:<br><a href="{link}" style="color:#6d28d9;word-break:break-all">{link}</a></p>
</td></tr>
<tr><td style="padding:16px 32px 32px;font-size:13px;color:#71717a;border-top:1px solid #e4e4e7">{footer}</td></tr>
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
    use super::*;

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
