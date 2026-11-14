use crate::config::HarnessConfig;
use crate::core::client::LlamaClient;
use crate::core::orchestrator::run_orchestrator;
use crate::core::subagent::run_disposable_subagent;
use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Cara 4: Servidor Proxy Agéntico OpenAI-Compatible (`http://<host>:<port>/v1`)
/// Permite que Neovim (`codecompanion.nvim`), Continue o cualquier cliente OpenAI invoque
/// a `oai-ti-manager` o a cualquiera de los subagentes (`oai-software-architect`, `oai-security-auditor`, etc.)
/// como si fueran modelos OpenAI estándar, ejecutando por debajo todo el bucle agéntico en Rust.
pub async fn run_openai_proxy_server(
    config: &HarnessConfig,
    workspace: &Path,
    host: &str,
    port: u16,
) -> Result<()> {
    let addr = format!("{}:{}", host, port);
    let listener = TcpListener::bind(&addr).await?;
    println!("================================================================================");
    println!("🌐 OPENED-AI OPENAI-COMPATIBLE AGENTIC PROXY ACTIVO");
    println!("================================================================================");
    println!(" • Escuchando en:  http://{}/v1", addr);
    if host == "0.0.0.0" {
        println!(" • Modo LAN activo (apunta tus clientes a http://<IP-DE-ESTE-HOST>:{}/v1)", port);
    }
    println!(" • Workspace:      {}", workspace.display());
    println!(" • Modelos Virtuales Expuestos en GET /v1/models:");
    let mut names: Vec<&String> = config.agents.keys().collect();
    names.sort();
    for n in names {
        println!("   - oai-{} (y alias directo `{}`)", n.replace('_', "-"), n);
    }
    println!("================================================================================\n");

    let shared_cfg = Arc::new(config.clone());
    let shared_ws = Arc::new(workspace.to_path_buf());

    loop {
        let (stream, _) = listener.accept().await?;
        let cfg = Arc::clone(&shared_cfg);
        let ws = Arc::clone(&shared_ws);
        tokio::spawn(async move {
            if let Err(e) = handle_http_connection(stream, &cfg, &ws).await {
                eprintln!("[WARN HTTP Proxy]: {}", e);
            }
        });
    }
}

async fn handle_http_connection(
    mut stream: TcpStream,
    config: &HarnessConfig,
    workspace: &Path,
) -> Result<()> {
    let mut buf = vec![0u8; 65536];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }

    let raw_req = String::from_utf8_lossy(&buf[..n]).to_string();
    let first_line = raw_req.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let method = parts[0];
    let path = parts[1];

    if method == "OPTIONS" {
        let resp = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\n\r\n";
        stream.write_all(resp.as_bytes()).await?;
        return Ok(());
    }

    if method == "GET" && (path.starts_with("/v1/models") || path == "/models") {
        let mut data = Vec::new();
        let mut names: Vec<&String> = config.agents.keys().collect();
        names.sort();
        for name in names {
            let virtual_id = format!("oai-{}", name.replace('_', "-"));
            data.push(json!({
                "id": virtual_id,
                "object": "model",
                "owned_by": "opened-ai-rust"
            }));
        }
        let body = json!({
            "object": "list",
            "data": data
        })
        .to_string();
        send_json_response(&mut stream, 200, &body).await?;
        return Ok(());
    }

    if method == "POST" && (path.starts_with("/v1/chat/completions") || path == "/chat/completions")
    {
        // Extraer el cuerpo JSON después de \r\n\r\n
        let body_str = raw_req
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or("{}")
            .trim_matches(char::from(0));
        let payload: Value = serde_json::from_str(body_str).unwrap_or_else(|_| json!({}));

        let requested_model = payload
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("oai-ti-manager");
        let is_stream = payload
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let user_prompt = payload
            .get("messages")
            .and_then(|m| m.as_array())
            .and_then(|arr| {
                arr.iter()
                    .rev()
                    .find(|msg| msg.get("role").and_then(|r| r.as_str()) == Some("user"))
            })
            .and_then(|msg| msg.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        let normalized_agent = requested_model
            .trim_start_matches("oai-")
            .trim_start_matches("lh-")
            .replace('-', "_");

        let client = LlamaClient::new(config.server.clone())?;
        let reply_content = if normalized_agent == "ti_manager"
            || !config.agents.contains_key(&normalized_agent)
        {
            match run_orchestrator(&client, config, workspace, &user_prompt).await {
                Ok(s) => s.final_response,
                Err(e) => format!("Error ejecutando TI Manager: {}", e),
            }
        } else {
            match run_disposable_subagent(
                &client,
                config,
                workspace,
                &normalized_agent,
                &user_prompt,
                None,
            )
            .await
            {
                Ok(r) => r.to_compact_prompt_block(),
                Err(e) => format!("Error ejecutando subagente `{}`: {}", normalized_agent, e),
            }
        };

        if is_stream {
            send_sse_response(&mut stream, requested_model, &reply_content).await?;
        } else {
            let resp_json = json!({
                "id": "chatcmpl-opened-ai",
                "object": "chat.completion",
                "created": 1731580000,
                "model": requested_model,
                "choices": [
                    {
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": reply_content
                        },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 0,
                    "completion_tokens": 0,
                    "total_tokens": 0
                }
            })
            .to_string();
            send_json_response(&mut stream, 200, &resp_json).await?;
        }
        return Ok(());
    }

    let not_found = json!({ "error": "Endpoint not found in opened-ai proxy" }).to_string();
    send_json_response(&mut stream, 404, &not_found).await?;
    Ok(())
}

async fn send_json_response(stream: &mut TcpStream, status: u16, body: &str) -> Result<()> {
    let status_text = if status == 200 { "OK" } else { "Not Found" };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        status_text,
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

async fn send_sse_response(stream: &mut TcpStream, model: &str, content: &str) -> Result<()> {
    let header = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n";
    stream.write_all(header.as_bytes()).await?;

    let chunk = json!({
        "id": "chatcmpl-opened-ai",
        "object": "chat.completion.chunk",
        "created": 1731580000,
        "model": model,
        "choices": [
            {
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "content": content
                },
                "finish_reason": null
            }
        ]
    });
    let done_chunk = json!({
        "id": "chatcmpl-opened-ai",
        "object": "chat.completion.chunk",
        "created": 1731580000,
        "model": model,
        "choices": [
            {
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }
        ]
    });

    let payload = format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n", chunk, done_chunk);
    stream.write_all(payload.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

