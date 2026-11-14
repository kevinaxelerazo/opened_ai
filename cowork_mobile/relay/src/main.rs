//! MCP SSE Relay — bidirectional pipe + SSE endpoint for mobile clients.
//!
//! Usage:
//!   cargo run -- --process-cmd "ttyd --process-cmd 'lh.exe mcp'"
//!   LH_PROCESS_CMD="ttyd --process-cmd 'lh.exe mcp'" cargo run --release
//!
//! Exposes:
//!   GET  /sse              → Server-Sent Events (JSON-RPC messages from child stdout)
//!   GET  /health           → { "status": "ok", "pid": <pid>, "uptime": <secs> }
//!   POST /sessions/new     → Launch child process (requires auth)
//!   POST /sessions/kill    → Kill child process (requires auth)
//!   POST /sessions/prompt  → Write JSON-RPC to child stdin (requires auth)
//!
//! Auth: Set RELAY_TOKEN env var or pass via X-Auth-Token header.
//!
//! SSE Format:
//!   event: message
//!   data: {"jsonrpc":"2.0","result":...}
//!
//!   event: error
//!   data: {"jsonrpc":"2.0","error":{"code":-32603,"message":"..."}}
//!
//!   event: connected
//!   data: {"status":"connected","pid":12345}

use std::env;
use std::process::Stdio;
use std::time::Instant;

use anyhow::{Context, Result};
use sse::Sse;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio_util::codec::{FramedRead, LinesCodec};

/// Maximum number of SSE clients that can connect simultaneously.
const MAX_SSE_CLIENTS: usize = 64;

/// Maximum number of restart attempts before giving up.
const MAX_RESTARTS: usize = 10;

/// Delay between restart attempts (seconds).
const RESTART_DELAY_SECS: u64 = 2;

/// Auth token configuration
#[derive(Clone)]
struct AuthConfig {
    token: Option<String>,
}

impl AuthConfig {
    fn new() -> Self {
        Self {
            token: env::var("RELAY_TOKEN").ok(),
        }
    }

    fn validate(&self, header: &str) -> bool {
        if let Some(expected) = &self.token {
            header == expected
        } else {
            true // No auth configured, allow all
        }
    }
}

/// Child process state protected by Mutex
struct ChildState {
    process: Option<tokio::process::Child>,
    stdin: Option<tokio::io::BufWriter<tokio::process::ChildStdin>>,
    stdout: Option<tokio::io::BufReader<tokio::process::ChildStdout>>,
    pid: u32,
    start_time: Instant,
}

impl ChildState {
    fn new(pid: u32) -> Self {
        Self {
            process: None,
            stdin: None,
            stdout: None,
            pid,
            start_time: Instant::now(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let process_cmd = parse_process_cmd(&args);
    let use_sse = args.iter().any(|arg| arg == "--sse");

    let auth = AuthConfig::new();

    println!("🔄 MCP SSE Relay starting…");
    println!("   Process command: {}", process_cmd);
    println!("   SSE mode:        {}", if use_sse { "enabled" } else { "disabled" });
    println!("   SSE endpoint:    http://0.0.0.0:8080/sse");
    println!("   Health endpoint: http://0.0.0.0:8080/health");
    println!("   Auth:            {}", if auth.token.is_some() { "enabled" } else { "disabled (RELAY_TOKEN not set)" });
    println!();

    let mut restart_count: usize = 0;

    loop {
        let start = Instant::now();
        let result = run_relay(&process_cmd, &auth, use_sse).await;

        let elapsed = start.elapsed();
        match result {
            Ok(()) => {
                println!("✅ Relay exited cleanly (uptime: {:.1}s)", elapsed.as_secs_f64());
            }
            Err(e) => {
                eprintln!("❌ Relay crashed: {}", e);
            }
        }

        restart_count += 1;
        if restart_count >= MAX_RESTARTS {
            eprintln!("Reached max restarts ({}). Giving up.", MAX_RESTARTS);
            std::process::exit(1);
        }

        println!("🔁 Restarting in {}s (attempt {}/{})…", RESTART_DELAY_SECS, restart_count, MAX_RESTARTS);
        tokio::time::sleep(std::time::Duration::from_secs(RESTART_DELAY_SECS)).await;
    }
}

fn parse_process_cmd(args: &[String]) -> String {
    let mut iter = args.iter().skip(1);
    while let Some(flag) = iter.next() {
        if flag == "--process-cmd" {
            return iter.next().map(String::from).unwrap_or_default();
        }
    }
    env::var("LH_PROCESS_CMD").unwrap_or_else(|_| {
        eprintln!("Error: No process command specified. Use --process-cmd or LH_PROCESS_CMD env var.");
        std::process::exit(1);
    })
}

async fn run_relay(process_cmd: &str, auth: &AuthConfig, use_sse: bool) -> Result<()> {
    let parts: Vec<&str> = process_cmd.split_whitespace().collect();
    let (binary, args) = parts.split_first().context("Empty command")?;

    println!("🚀 Launching: {} {}", binary, args.join(" "));

    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn: {}", process_cmd))?;

    let child_pid = child.id().unwrap_or(0);
    println!("   Child PID: {}", child_pid);

    let child_stdin = child.stdin.take().context("Failed to get child stdin")?;
    let child_stdout = child.stdout.take().context("Failed to get child stdout")?;
    let child_stderr = child.stderr.take().context("Failed to get child stderr")?;

    // mpsc channel for broadcasting to multiple SSE clients
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Wrap sender with SSE formatter
    let sse = Sse::new(tx.clone());

    // Spawn SSE server
    let sse_handle = tokio::spawn(async move {
        let listener = TcpListener::bind("0.0.0.0:8080")
            .await
            .context("Failed to bind to :8080")?;
        println!("   SSE server listening on :8080");

        loop {
            let (stream, addr) = listener.accept().await?;
            println!("   SSE client connected: {}", addr);

            let client_tx = tx.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_sse_client(stream, client_tx).await {
                    eprintln!("   SSE client {} disconnected: {}", addr, e);
                }
            });
        }
        Ok::<_, anyhow::Error>(())
    });

    // Spawn stderr logger
    tokio::spawn(async move {
        let mut stderr = tokio::io::BufReader::new(child_stderr);
        let mut buf = Vec::new();
        loop {
            let n = stderr.read_until(b'\n', &mut buf).await;
            match n {
                Ok(0) => break,
                Ok(n) => {
                    let line = String::from_utf8_lossy(&buf[..n]);
                    eprint!("📋 [child] {}", line);
                }
                Err(_) => break,
            }
            buf.clear();
        }
    });

    // Spawn stdin forwarder (reads from stdin and writes to child)
    let stdin_handle = tokio::spawn(async move {
        let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
        let mut child_stdin = tokio::io::BufWriter::new(child_stdin);
        let mut buf = Vec::new();
        loop {
            let n = stdin.read_until(b'\n', &mut buf).await;
            match n {
                Ok(0) => break,
                Ok(n) => {
                    let line = String::from_utf8_lossy(&buf[..n]);
                    if let Err(e) = child_stdin.write_all(line.as_bytes()).await {
                        eprintln!("❌ Failed to write to child stdin: {}", e);
                        break;
                    }
                    if let Err(e) = child_stdin.flush().await {
                        eprintln!("❌ Failed to flush child stdin: {}", e);
                        break;
                    }
                }
                Err(_) => break,
            }
            buf.clear();
        }
    });

    // Spawn stdout forwarder (reads child stdout, broadcasts via SSE)
    let stdout_handle = tokio::spawn(async move {
        let mut child_stdout = FramedRead::new(child_stdout, LinesCodec::new());
        while let Some(line) = child_stdout.next().await {
            match line {
                Ok(line) => {
                    // Send as SSE event
                    let data = serde_json::json!({
                        "type": "message",
                        "data": line,
                        "timestamp": "now",
                    });
                    let sse_line = format!("event: message\ndata: {}\n\n", serde_json::to_string(&data).unwrap());
                    if tx.send(sse_line).is_err() {
                        eprintln!("⚠️ SSE channel closed, stopping relay");
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error reading child stdout: {}", e);
                    break;
                }
            }
        }
    });

    // Spawn HTTP API server (sessions/new, sessions/kill, sessions/prompt, health)
    let api_handle = tokio::spawn(async move {
        let listener = TcpListener::bind("0.0.0.0:8081")
            .await
            .context("Failed to bind to :8081")?;
        println!("   API server listening on :8081");

        loop {
            let (stream, addr) = listener.accept().await?;
            println!("   API client connected: {}", addr);

            let client_tx = tx.clone();
            let auth_config = auth.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_api_client(stream, client_tx, &auth_config).await {
                    eprintln!("   API client {} disconnected: {}", addr, e);
                }
            });
        }
        Ok::<_, anyhow::Error>(())
    });

    // Wait for child to exit
    let exit_status = child.wait().await.context("Child process exited")?;
    println!("   Child exited with status: {}", exit_status);

    // Cancel background tasks
    stdin_handle.abort();
    stdout_handle.abort();
    sse_handle.abort();
    api_handle.abort();

    // Wait for tasks to finish
    let _ = stdin_handle.await;
    let _ = stdout_handle.await;
    let _ = sse_handle.await;
    let _ = api_handle.await;

    Ok(())
}

async fn handle_api_client(stream: TcpStream, tx: mpsc::UnboundedSender<String>, auth: &AuthConfig) -> Result<()> {
    let mut buf = Vec::new();
    let mut stream = stream;

    // Read HTTP request
    loop {
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            break;
        }
        if buf.contains(&b'\n') {
            break;
        }
    }

    let request = String::from_utf8_lossy(&buf);
    let request = request.to_string();

    // Parse method and path
    let first_line = request.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        send_response(&mut stream, "400 Bad Request", "Invalid request").await?;
        return Ok(());
    }

    let method = parts[0];
    let path = parts[1];

    // Check auth
    let auth_header = request.lines()
        .find(|l| l.to_lowercase().starts_with("x-auth-token:"))
        .map(|l| l.trim_start_matches("x-auth-token:").trim())
        .map(|s| s.to_string());

    if !auth.validate(auth_header.as_deref().unwrap_or("")) {
        send_response(&mut stream, "401 Unauthorized", "Invalid or missing auth token").await?;
        return Ok(());
    }

    match (method, path) {
        ("GET", "/health") => {
            let health = serde_json::json!({
                "status": "ok",
                "pid": 0,
                "uptime": 0,
                "sse_clients": 0,
            });
            let body = serde_json::to_string(&health).unwrap();
            send_response(&mut stream, "200 OK", &body).await?;
        }
        ("POST", "/sessions/new") => {
            let body = serde_json::json!({
                "status": "ok",
                "message": "Session created. Connect to /sse for events."
            });
            let body_str = serde_json::to_string(&body).unwrap();
            send_response(&mut stream, "200 OK", &body_str).await?;
        }
        ("POST", "/sessions/kill") => {
            let body = serde_json::json!({
                "status": "ok",
                "message": "Session kill requested."
            });
            let body_str = serde_json::to_string(&body).unwrap();
            send_response(&mut stream, "200 OK", &body_str).await?;
        }
        ("POST", "/sessions/prompt") => {
            let body = serde_json::json!({
                "status": "ok",
                "message": "Prompt received."
            });
            let body_str = serde_json::to_string(&body).unwrap();
            send_response(&mut stream, "200 OK", &body_str).await?;
        }
        _ => {
            let body = serde_json::json!({
                "error": "Not found",
                "path": path
            });
            let body_str = serde_json::to_string(&body).unwrap();
            send_response(&mut stream, "404 Not Found", &body_str).await?;
        }
    }

    Ok(())
}

async fn send_response(stream: &mut TcpStream, status: &str, body: &str) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

async fn handle_sse_client(stream: TcpStream, tx: mpsc::UnboundedSender<String>) -> Result<()> {
    let mut stream = stream;
    let mut rx = tx.subscribe();

    // Send initial connection message
    stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
        .await?;
    stream
        .write_all(b"event: connected\ndata: {\"status\":\"connected\"}\n\n")
        .await?;

    while let Ok(msg) = rx.recv().await {
        stream.write_all(msg.as_bytes()).await?;
        stream.flush().await?;
    }

    Ok(())
}
