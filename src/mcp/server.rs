use super::protocol::*;
use super::tools;
use crate::api_client::APIClient;
use crate::lsp;
use anyhow::Result;
use serde_json::{json, Value};

pub struct Server {
    pub api: APIClient,
    pub lsp: lsp::LSPManager,
    pub default_project_id: Option<String>,
    pub workspace_root: std::path::PathBuf,
}

impl Server {
    pub fn new(api: APIClient, lsp: lsp::LSPManager) -> Self {
        let workspace_root = lsp.workspace_root().to_path_buf();
        Self { api, lsp, default_project_id: None, workspace_root }
    }

    pub fn with_default_project(mut self, project_id: Option<String>) -> Self {
        self.default_project_id = project_id;
        self
    }

    /// Handle incoming MCP request
    pub fn handle_request(&mut self, line: &str) -> Result<Option<String>> {
        let req: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                // JSON-RPC spec: parse errors must return code -32700
                return Ok(Some(self.error_response(None, -32700, &format!("Parse error: {}", e))));
            }
        };

        tracing::debug!("Received request: method={}", req.method);

        // Per JSON-RPC/MCP, notifications should not receive responses.
        let response = match req.method.as_str() {
            "initialize" => Some(self.handle_initialize(&req)),
            "tools/list" => Some(self.handle_tools_list(&req)),
            "tools/call" => Some(self.handle_tools_call(&req)),
            "notifications/initialized" => None,
            _ => {
                if req.method.starts_with("notifications/") {
                    None
                } else {
                    let error = ErrorObject {
                        code: -32601,
                        message: format!("Method not found: {}", req.method),
                        data: None,
                    };
                    Some(Response {
                        jsonrpc: "2.0".to_string(),
                        id: req.id.clone(),
                        result: None,
                        error: Some(error),
                    })
                }
            }
        };

        match response {
            Some(resp) => Ok(Some(serde_json::to_string(&resp)?)),
            None => Ok(None),
        }
    }

    /// Handle initialize request
    fn handle_initialize(&self, req: &Request) -> Response {
        let result = InitializeResult {
            protocol_version: "2024-11-05".to_string(),
            capabilities: Capabilities {
                tools: json!({
                    "listChanged": false
                }),
            },
            server_info: ServerInfo {
                name: "cultra".to_string(),
                version: "1.0.0".to_string(),
            },
        };

        Response {
            jsonrpc: "2.0".to_string(),
            id: req.id.clone(),
            result: serde_json::to_value(result).ok(),
            error: None,
        }
    }

    /// Handle tools/list request
    fn handle_tools_list(&self, req: &Request) -> Response {
        let tools = tools::get_tool_definitions();

        Response {
            jsonrpc: "2.0".to_string(),
            id: req.id.clone(),
            result: Some(json!({ "tools": tools })),
            error: None,
        }
    }

    /// Handle tools/call request
    fn handle_tools_call(&mut self, req: &Request) -> Response {
        let params = match req.params.as_object() {
            Some(p) => p,
            None => {
                return self.error_response_obj(
                    req.id.clone(),
                    -32602,
                    "Invalid params: expected object",
                )
            }
        };

        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => {
                return self.error_response_obj(
                    req.id.clone(),
                    -32602,
                    "Missing 'name' parameter",
                )
            }
        };

        let args = params
            .get("arguments")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();

        tracing::info!("Calling tool: {}", name);

        match tools::call_tool(self, name, args) {
            Ok(result) => {
                // Wrap result in MCP content format.
                // result is already a serde_json::Value — convert to compact string
                // for the text content field (MCP spec: text content is a string).
                let text = match result {
                    Value::String(s) => s,
                    other => other.to_string(),
                };
                let content = json!({
                    "content": [
                        {
                            "type": "text",
                            "text": text
                        }
                    ]
                });

                Response {
                    jsonrpc: "2.0".to_string(),
                    id: req.id.clone(),
                    result: Some(content),
                    error: None,
                }
            },
            Err(e) => {
                tracing::error!("Tool execution error: {}", e);
                self.error_response_obj(req.id.clone(), -32000, &e.to_string())
            }
        }
    }

    /// Create error response
    pub fn error_response(&self, id: Option<Value>, code: i32, message: &str) -> String {
        let response = self.error_response_obj(id, code, message);
        serde_json::to_string(&response).unwrap_or_else(|_| {
            let escaped_message = serde_json::to_string(message).unwrap_or_else(|_| "\"internal error\"".to_string());
            format!(
                r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":{},"message":{}}}}}"#,
                code, escaped_message
            )
        })
    }

    fn error_response_obj(&self, id: Option<Value>, code: i32, message: &str) -> Response {
        Response {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(ErrorObject {
                code,
                message: message.to_string(),
                data: None,
            }),
        }
    }
}
