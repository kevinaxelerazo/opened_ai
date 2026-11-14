use crate::config::HarnessConfig;
use crate::core::client::LlamaClient;
use crate::core::orchestrator::run_orchestrator;
use crate::core::subagent::run_disposable_subagent;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::Path;

/// Cara 3: Servidor ACP (Agent Client Protocol JSON-RPC 2.0 sobre `stdio`)
/// Diseñado para integrarse como adaptador ACP en Neovim (`codecompanion.nvim`) y Zed.
pub async fn run_acp_stdio_server(config: &HarnessConfig, workspace: &Path) -> Result<()> {
    let client = LlamaClient::new(config.server.clone())?;
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line_res in stdin.lock().lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("ACP JSON parse error: {}", e) }
                });
                writeln!(stdout, "{}", err)?;
                stdout.flush()?;
                continue;
            }
        };

        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

        if method.starts_with("notifications/") {
            continue;
        }

        let resp = match method {
            "initialize" | "acp/initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "0.1.0",
                    "agentInfo": {
                        "name": "opened-ai-acp",
                        "version": "0.1.0",
                        "agents": config.agents.keys().collect::<Vec<_>>()
                    },
                    "capabilities": {
                        "disposableSubagents": true,
                        "multimodalVision": true,
                        "webRag": true
                    }
                }
            }),
            "session/new" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "sessionId": "oai-session-01",
                    "workspace": workspace.display().to_string()
                }
            }),
            "session/prompt" | "agent/prompt" | "chat/prompt" => {
                let prompt_text = params
                    .get("prompt")
                    .or_else(|| params.get("message"))
                    .or_else(|| params.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();

                let explicit_agent = params.get("agent").and_then(|v| v.as_str());

                let reply_text = if let Some(ag) = explicit_agent {
                    if ag == "ti_manager" {
                        match run_orchestrator(&client, config, workspace, prompt_text).await {
                            Ok(s) => s.final_response,
                            Err(e) => format!("Error en TI Manager: {}", e),
                        }
                    } else {
                        match run_disposable_subagent(&client, config, workspace, ag, prompt_text, None).await {
                            Ok(r) => r.to_compact_prompt_block(),
                            Err(e) => format!("Error en subagente `{}`: {}", ag, e),
                        }
                    }
                } else if let Some(rest) = prompt_text.strip_prefix('@') {
                    let mut sp = rest.splitn(2, char::is_whitespace);
                    if let (Some(ag), Some(task)) = (sp.next(), sp.next()) {
                        match run_disposable_subagent(&client, config, workspace, ag.trim(), task.trim(), None).await {
                            Ok(r) => r.to_compact_prompt_block(),
                            Err(e) => format!("Error en subagente `{}`: {}", ag, e),
                        }
                    } else {
                        "Uso: `@nombre_subagente <instrucción>`".to_string()
                    }
                } else {
                    match run_orchestrator(&client, config, workspace, prompt_text).await {
                        Ok(s) => s.final_response,
                        Err(e) => format!("Error en TI Manager: {}", e),
                    }
                };

                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "stopReason": "end_turn",
                        "content": [
                            {
                                "type": "text",
                                "text": reply_text
                            }
                        ]
                    }
                })
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Método ACP no soportado: {}", method)
                }
            }),
        };

        writeln!(stdout, "{}", resp)?;
        stdout.flush()?;
    }

    Ok(())
}

