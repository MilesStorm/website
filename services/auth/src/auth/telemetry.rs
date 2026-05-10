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
