use std::time::Duration;

use reqwest::Client;

use crate::{truncate, Config};

// ── http_fetch ───────────────────────────────────────────────────────────────

pub(crate) async fn tool_http_fetch(
    args: &serde_json::Value,
    http: &Client,
    config: &Config,
) -> String {
    let url = match args["url"].as_str() {
        Some(u) => u,
        None => return "Error: 'url' parameter is required".into(),
    };
    let max_bytes = args["max_bytes"].as_u64().unwrap_or(102_400) as usize;

    let result = tokio::time::timeout(Duration::from_secs(15), http.get(url).send()).await;

    match result {
        Ok(Ok(resp)) => {
            let status = resp.status();
            let content_type = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_string();
            match resp.text().await {
                Ok(text) => {
                    let header = format!("HTTP {status} | {content_type}\n---\n");
                    truncate(&format!("{header}{text}"), max_bytes.min(config.max_output_bytes))
                }
                Err(e) => format!("http_fetch error reading body: {e}"),
            }
        }
        Ok(Err(e)) => format!("http_fetch error: {e}"),
        Err(_) => "http_fetch error: request timed out (15s)".into(),
    }
}
