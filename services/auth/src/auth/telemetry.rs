pub fn login_attempt(method: &str, status: &str) {
    metrics::counter!(
        "auth_login_attempts_total",
        "method" => method.to_string(),
        "status" => status.to_string()
    )
    .increment(1);
}

pub fn token_operation(operation: &str, status: &str) {
    metrics::counter!(
        "auth_token_operations_total",
        "operation" => operation.to_string(),
        "status" => status.to_string()
    )
    .increment(1);
}

pub fn ark_command(cmd: &str, status: &str) {
    metrics::counter!(
        "auth_ark_commands_total",
        "cmd" => cmd.to_string(),
        "status" => status.to_string()
    )
    .increment(1);
}

/// An account email: `kind` is verify_email / reset_password / delete_account,
/// `status` is sent / failed.
pub fn email(kind: &str, status: &str) {
    metrics::counter!(
        "auth_emails_total",
        "kind" => kind.to_string(),
        "status" => status.to_string()
    )
    .increment(1);
}

/// A span for a task that outlives the request (`tokio::spawn`): in the request's trace,
/// but parented through OTel only, so the request span still closes (and is exported) when
/// the response goes out. `.in_current_span()` would hold it open until the task ends.
pub fn detached_span(name: &'static str) -> tracing::Span {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    let span = tracing::info_span!(parent: None, "background", otel.name = name);
    span.set_parent(tracing::Span::current().context());
    span
}
