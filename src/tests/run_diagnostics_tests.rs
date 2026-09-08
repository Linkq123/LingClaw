use super::*;

#[test]
fn provider_diagnostics_use_only_the_owned_transport_prefix() {
    for (error, expected) in [
        (
            "API 401 Unauthorized: sensitive body",
            RunDiagnosticCode::ProviderAuthentication,
        ),
        (
            "API 403 Forbidden: sensitive body",
            RunDiagnosticCode::ProviderAuthentication,
        ),
        (
            "API 429 Too Many Requests (after 2 attempts): body",
            RunDiagnosticCode::ProviderRateLimited,
        ),
        (
            "API 500 Internal Server Error: API 401 misleading body",
            RunDiagnosticCode::ProviderUnavailable,
        ),
        (
            "API 400 Bad Request: <html>401 token</html>",
            RunDiagnosticCode::ProviderRequestRejected,
        ),
        (
            "HTTP error: request URL with credentials",
            RunDiagnosticCode::ProviderConnection,
        ),
        (
            "Provider configuration error: bad header",
            RunDiagnosticCode::ModelConfiguration,
        ),
        (
            "OpenAI API error: API 401 Unauthorized",
            RunDiagnosticCode::ProviderResponseInvalid,
        ),
        (
            "API 401evil body",
            RunDiagnosticCode::ProviderResponseInvalid,
        ),
    ] {
        let diagnostic = RunDiagnostic::from_provider_error(error);
        assert_eq!(diagnostic.code, expected);
        assert_eq!(
            RunDiagnostic::from_reason(Some(diagnostic.reason())),
            Some(diagnostic)
        );
        let serialized = serde_json::to_string(&diagnostic).unwrap();
        assert!(serialized.len() < 64);
        assert!(!serialized.contains("sensitive"));
        assert!(!diagnostic.safe_message().contains(error));
    }
}

#[test]
fn unknown_diagnostics_and_extra_raw_fields_are_rejected() {
    assert!(RunDiagnostic::from_reason(Some("old_provider_error")).is_none());
    assert!(RunDiagnostic::from_reason(None).is_none());
    assert!(
        serde_json::from_str::<RunDiagnostic>(r#"{"code":"<script>secret</script>"}"#).is_err()
    );
    assert!(
        serde_json::from_str::<RunDiagnostic>(r#"{"code":"provider_unavailable","body":"secret"}"#)
            .is_err()
    );
    let raw = format!(
        "API 500 Internal Server Error: {}",
        "<html>token=secret https://key:secret@example.test/path</html>".repeat(10000)
    );
    let diagnostic = RunDiagnostic::from_provider_error(&raw);
    assert_eq!(diagnostic.code, RunDiagnosticCode::ProviderUnavailable);
    assert!(!diagnostic.safe_message().contains("secret"));
    assert!(diagnostic.safe_message().len() < 240);
}
