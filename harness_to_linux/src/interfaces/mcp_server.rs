use crate::config::HarnessConfig;
use crate::core::client::LlamaClient;
use crate::core::orchestrator::run_orchestrator;
use crate::core::subagent::run_disposable_subagent;
use crate::skills::format_skills_catalog;
use crate::tools::vision_cdp::tool_vision_inspect;
use crate::tools::web_rag::tool_web_rag;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::Path;

/// Cara 2: Servidor MCP (Model Context Protocol JSON-RPC 2.0 sobre `stdio`)
/// Permite conectar `lh.exe mcp` directamente a Antigravity IDE, Claude Desktop o Cursor.
pub async fn run_mcp_stdio_server(config: &HarnessConfig, workspace: &Path) -> Result<()> {
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
                let err_resp = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("Parse error: {}", e) }
                });
                writeln!(stdout, "{}", err_resp)?;
                stdout.flush()?;
                continue;
            }
        };

        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

        // Las notificaciones JSON-RPC (sin `id` o `notifications/*`) no requieren respuesta
        if method.starts_with("notifications/") {
            continue;
        }

        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "opened-ai-mcp",
                        "version": "0.1.0"
                    }
                }
            }),
            "ping" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": mcp_tools_catalog()
                }
            }),
            "tools/call" => {
                let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
                let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

                let (is_error, text_out) =
                    handle_mcp_tool_call(tool_name, &args, &client, config, workspace).await;

                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": text_out
                            }
                        ],
                        "isError": is_error
                    }
                })
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {}", method)
                }
            }),
        };

        writeln!(stdout, "{}", response)?;
        stdout.flush()?;
    }

    Ok(())
}

fn mcp_tools_catalog() -> Vec<Value> {
    vec![
        json!({
            "name": "oai_delegate_subagent",
            "description": "Invoca un subagente local especializado de Opened-AI con contexto desechable (explorer, software_architect, semi_sr_dev, qa_tester, web_researcher, frontend_inspector, security_auditor).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent_name": {
                        "type": "string",
                        "description": "Nombre del subagente en config.yaml"
                    },
                    "task_brief": {
                        "type": "string",
                        "description": "Tarea autocontenida para el subagente local"
                    },
                    "context_hints": {
                        "type": "string",
                        "description": "Rutas de archivos o contexto relevante opcional"
                    }
                },
                "required": ["agent_name", "task_brief"]
            }
        }),
        json!({
            "name": "oai_run_ti_manager",
            "description": "Ejecuta un flujo completo orquestado por el TI Manager local (Qwen 3.8 Distill) coordinando subagentes secuenciales.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "goal": {
                        "type": "string",
                        "description": "Objetivo de ingeniería para el TI Manager"
                    }
                },
                "required": ["goal"]
            }
        }),
        json!({
            "name": "oai_vision_inspect",
            "description": "Captura pantallas multi-viewport con Chrome/Edge Headless y ejecuta telemetría DOM + análisis visual multimodal con Gemma 4 26B (mmproj-F16).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "URL, ruta a archivo .html local o ruta a imagen .png/.jpg"
                    },
                    "viewport": {
                        "type": "string",
                        "description": "desktop (1440x900), tablet (768x1024) o mobile (375x812)"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Aspecto visual a verificar"
                    }
                },
                "required": ["target"]
            }
        }),
        json!({
            "name": "oai_web_rag",
            "description": "Búsqueda técnica comprimida en la web o extracción de documentación URL a Markdown limpio.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Consulta técnica a buscar" },
                    "url": { "type": "string", "description": "URL directa opcional" }
                }
            }
        }),
        json!({
            "name": "oai_catalog",
            "description": "Lista los agentes locales configurados y las Skills especializadas instaladas en Opened-AI.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
    ]
}

async fn handle_mcp_tool_call(
    tool_name: &str,
    args: &Value,
    client: &LlamaClient,
    config: &HarnessConfig,
    workspace: &Path,
) -> (bool, String) {
    match tool_name {
        "oai_delegate_subagent" | "lh_delegate_subagent" => {
            let agent = args.get("agent_name").and_then(|v| v.as_str()).unwrap_or("explorer");
            let task = args.get("task_brief").and_then(|v| v.as_str()).unwrap_or("");
            let hints = args.get("context_hints").and_then(|v| v.as_str());
            match run_disposable_subagent(client, config, workspace, agent, task, hints).await {
                Ok(rep) => (false, rep.to_compact_prompt_block()),
                Err(e) => (true, format!("Error en subagente `{}`: {}", agent, e)),
            }
        }
        "oai_run_ti_manager" | "lh_run_ti_manager" => {
            let goal = args.get("goal").and_then(|v| v.as_str()).unwrap_or("");
            match run_orchestrator(client, config, workspace, goal).await {
                Ok(sum) => (false, sum.final_response),
                Err(e) => (true, format!("Error en TI Manager: {}", e)),
            }
        }
        "oai_vision_inspect" | "lh_vision_inspect" => {
            let out = tool_vision_inspect(args, workspace, &config.server.base_url).await;
            (false, out)
        }
        "oai_web_rag" | "lh_web_rag" => {
            let out = tool_web_rag(args).await;
            (false, out)
        }
        "oai_catalog" | "lh_catalog" => {
            let mut out = String::from("⚡ OPENED-AI AGENTES LOCALES CONFIGURADOS:\n");
            let mut names: Vec<&String> = config.agents.keys().collect();
            names.sort();
            for n in names {
                if let Some(a) = config.agents.get(n) {
                    out.push_str(&format!("- `{}` ({}) -> Modelo: `{}`\n", n, a.role, a.model));
                }
            }
            out.push('\n');
            out.push_str(&format_skills_catalog(workspace));
            (false, out)
        }
        _ => (true, format!("Herramienta MCP desconocida: `{}`", tool_name)),
    }
}

