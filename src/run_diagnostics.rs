use serde::{Deserialize, Serialize};

/// A closed diagnostic vocabulary. Persist only its stable code in the existing
/// terminal reason column; text, URLs and Provider bodies are never durable data.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunDiagnosticCode {
    ProviderAuthentication,
    ProviderRateLimited,
    ProviderUnavailable,
    ProviderConnection,
    ProviderRequestRejected,
    ProviderResponseInvalid,
    ModelConfiguration,
    ContextBudgetExceeded,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunDiagnostic {
    pub(crate) code: RunDiagnosticCode,
}

impl RunDiagnostic {
    pub(crate) fn from_reason(reason: Option<&str>) -> Option<Self> {
        let code = match reason? {
            "provider_authentication" => RunDiagnosticCode::ProviderAuthentication,
            "provider_rate_limited" => RunDiagnosticCode::ProviderRateLimited,
            "provider_unavailable" => RunDiagnosticCode::ProviderUnavailable,
            "provider_connection" => RunDiagnosticCode::ProviderConnection,
            "provider_request_rejected" => RunDiagnosticCode::ProviderRequestRejected,
            "provider_response_invalid" => RunDiagnosticCode::ProviderResponseInvalid,
            "model_configuration" => RunDiagnosticCode::ModelConfiguration,
            "context_budget_exceeded" => RunDiagnosticCode::ContextBudgetExceeded,
            _ => return None,
        };
        Some(Self { code })
    }

    pub(crate) fn reason(self) -> &'static str {
        match self.code {
            RunDiagnosticCode::ProviderAuthentication => "provider_authentication",
            RunDiagnosticCode::ProviderRateLimited => "provider_rate_limited",
            RunDiagnosticCode::ProviderUnavailable => "provider_unavailable",
            RunDiagnosticCode::ProviderConnection => "provider_connection",
            RunDiagnosticCode::ProviderRequestRejected => "provider_request_rejected",
            RunDiagnosticCode::ProviderResponseInvalid => "provider_response_invalid",
            RunDiagnosticCode::ModelConfiguration => "model_configuration",
            RunDiagnosticCode::ContextBudgetExceeded => "context_budget_exceeded",
        }
    }

    /// Accept only the Provider adapter's error envelope. Bare transport
    /// prefixes are reserved for send_with_retry; adapters must wrap every
    /// upstream field (including root SSE messages) behind a fixed protocol
    /// label. Never scan that wrapped body for status numbers, words or URLs.
    pub(crate) fn from_provider_error(error: &str) -> Self {
        let status = error.strip_prefix("API ").and_then(|text| {
            if text.as_bytes().get(3) != Some(&b' ') {
                return None;
            }
            text.get(..3)?.parse::<u16>().ok()
        });
        let code = match status {
            Some(401 | 403) => RunDiagnosticCode::ProviderAuthentication,
            Some(429) => RunDiagnosticCode::ProviderRateLimited,
            Some(408 | 500..=599) => RunDiagnosticCode::ProviderUnavailable,
            Some(400..=499) => RunDiagnosticCode::ProviderRequestRejected,
            _ if error.starts_with("Provider configuration error: ") => {
                RunDiagnosticCode::ModelConfiguration
            }
            _ if error.starts_with("HTTP error: ") => RunDiagnosticCode::ProviderConnection,
            _ => RunDiagnosticCode::ProviderResponseInvalid,
        };
        Self { code }
    }

    pub(crate) fn safe_message(self) -> &'static str {
        match self.code {
            RunDiagnosticCode::ProviderAuthentication => {
                "The model provider rejected authentication. Review its credentials in Models."
            }
            RunDiagnosticCode::ProviderRateLimited => {
                "The model provider rate-limited this request. Wait before trying again."
            }
            RunDiagnosticCode::ProviderUnavailable => {
                "The model provider is temporarily unavailable. Try again later."
            }
            RunDiagnosticCode::ProviderConnection => {
                "The model provider could not be reached. Check connectivity and the Provider address."
            }
            RunDiagnosticCode::ProviderRequestRejected => {
                "The model provider rejected the request. Review the model and protocol configuration."
            }
            RunDiagnosticCode::ProviderResponseInvalid => {
                "The model provider returned an unusable response. Review the model and protocol configuration."
            }
            RunDiagnosticCode::ModelConfiguration => {
                "The model request could not be constructed. Review the Provider address and credentials."
            }
            RunDiagnosticCode::ContextBudgetExceeded => {
                "The request exceeds the model context budget. Reduce the context or choose a larger-context model."
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/run_diagnostics_tests.rs"]
mod tests;
