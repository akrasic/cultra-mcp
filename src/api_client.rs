use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use ureq::http;

/// API error response structure (matches server ErrorResponse)
#[derive(Debug, Deserialize)]
struct ErrorResponse {
    #[serde(rename = "error")]
    message: String,
    code: String,
    #[serde(default)]
    details: Option<String>,
}

/// HTTP client for Cultra API (truly synchronous with ureq)
pub struct APIClient {
    agent: ureq::Agent,
    base_url: String,
    api_key: String,
}

impl APIClient {
    pub fn new(base_url: String, api_key: String) -> Result<Self> {
        // ureq v3: Use Agent::config_builder() instead of AgentBuilder
        // Set http_status_as_error(false) so we can parse error response bodies
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)  // Allow us to parse error response bodies
            .build()
            .into();

        Ok(Self {
            agent,
            base_url,
            api_key,
        })
    }

    /// Execute GET request
    pub fn get(&self, path: &str, query: Option<Vec<(String, String)>>) -> Result<Value> {
        let mut url = format!("{}{}", self.base_url, path);

        // Add query parameters
        if let Some(params) = query {
            if !params.is_empty() {
                url.push('?');
                for (i, (key, value)) in params.iter().enumerate() {
                    if i > 0 {
                        url.push('&');
                    }
                    url.push_str(&urlencoding::encode(key));
                    url.push('=');
                    url.push_str(&urlencoding::encode(value));
                }
            }
        }

        let response = self.agent
            .get(&url)
            .header("Authorization", &self.get_auth_header())
            .call()?;

        self.parse_response(response)
    }

    /// Execute POST request
    pub fn post(&self, path: &str, body: Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);

        let response = self.agent
            .post(&url)
            .header("Authorization", &self.get_auth_header())
            .header("Content-Type", "application/json")
            .send_json(&body)?;

        self.parse_response(response)
    }

    /// Execute PUT request
    pub fn put(&self, path: &str, body: Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);

        let response = self.agent
            .put(&url)
            .header("Authorization", &self.get_auth_header())
            .header("Content-Type", "application/json")
            .send_json(&body)?;

        self.parse_response(response)
    }

    /// Execute DELETE request
    pub fn delete(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);

        let response = self.agent
            .delete(&url)
            .header("Authorization", &self.get_auth_header())
            .call()?;

        self.parse_response(response)
    }

    /// Get authorization header value
    fn get_auth_header(&self) -> String {
        if self.api_key.starts_with("sk_") {
            // API key - send directly
            self.api_key.clone()
        } else {
            // JWT token - use Bearer prefix
            format!("Bearer {}", self.api_key)
        }
    }

    /// Parse HTTP response with detailed error messages
    fn parse_response(&self, mut response: http::Response<ureq::Body>) -> Result<Value> {
        let status = response.status().as_u16();

        if status >= 200 && status < 300 {
            // Success: parse JSON response
            let body: Value = response.body_mut().read_json()
                .context("Failed to parse JSON response")?;
            return Ok(body);
        }

        // Error: parse error response body
        let body_text = response.body_mut().read_to_string()
            .unwrap_or_else(|_| "{}".to_string());

        // Try to parse structured error response
        if let Ok(err_resp) = serde_json::from_str::<ErrorResponse>(&body_text) {
            // Map HTTP status codes to human-friendly context
            let context = match status {
                400 => "Invalid request",
                404 => "Not found or access denied",
                409 => "Conflict",
                500 => "Server error",
                _ => "Request failed",
            };

            // Build detailed error message
            let mut msg = format!("{}: {}", context, err_resp.message);
            if let Some(details) = err_resp.details {
                if !details.is_empty() {
                    msg.push_str(&format!(" - {}", details));
                }
            }

            anyhow::bail!("{}", msg);
        }

        // Fallback for non-JSON error responses
        anyhow::bail!("HTTP {}: {}", status, body_text);
    }
}
