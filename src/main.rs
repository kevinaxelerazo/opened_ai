mod config;
mod core;
mod interfaces;
mod skills;
mod tools;

use crate::config::HarnessConfig;
use crate::core::client::LlamaClient;
use crate::core::orchestrator::run_orchestrator;
use crate::core::subagent::run_disposable_subagent;
use crate::interfaces::acp_server::run_acp_stdio_server;
use crate::interfaces::chat_tui::run_interactive_chat;
use crate::interfaces::mcp_server::run_mcp_stdio_server;
use crate::interfaces::openai_proxy::run_openai_proxy_server;
use crate::skills::discover_skills;
use crate::tools::vision_cdp::tool_vision_inspect;
use crate::tools::web_rag::tool_web_rag;
use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "oai",
    version = "0.1.0",
    about = "⚡ Opened-AI: Ultra-Light Rust Agentic Framework for Local MoE LLMs (ik_llama.cpp + llama-swap)"
)]
struct Cli {
    /// Ruta opcional a config.yaml o config.json
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Directorio del proyecto sobre el que actuar (default: directorio actual)
    #[arg(short, long)]
    workspace: Option<PathBuf>,

    /// Sobreescribe IP/URL del servidor llama-swap (ej: 192.168.18.10 o http://192.168.18.10:8080/v1)
    #[arg(short, long)]
    server: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Lista todos los agentes y subagentes definidos en config/config.yaml
    Agents,

    /// Lista todas las Skills de carga perezosa instaladas (skills/*/SKILL.md)
    Skills,

    /// Verifica el estado de conexión con llama-swap / ik_llama.cpp y los modelos disponibles
    Status,

    /// Ejecuta una tarea completa liderada por el TI Manager (Orquestador + Subagentes de Contexto Desechable)
    Run {
        /// Instrucción u objetivo para el TI Manager
        goal: String,
    },

    /// Invoca directamente a un subagente específico con contexto desechable (ej. explorer, semi_sr_dev, qa_tester)
    Subagent {
        /// Nombre del agente en config/config.yaml
        agent_name: String,
        /// Tarea puntual para el subagente
        task: String,
    },

    /// Ejecuta una consulta Web RAG comprimida o extrae Markdown limpio de una URL
    WebRag {
        /// Consulta de búsqueda o URL (si empieza con http:// o https://)
        query_or_url: String,
    },

    /// Captura pantalla con Chrome/Edge Headless y ejecuta auditoría DOM + Visión Multimodal (`mmproj-F16`)
    Vision {
        /// URL, ruta a archivo .html local o ruta a imagen .png/.jpg
        target: String,
        /// Viewport: desktop (default), tablet o mobile
        #[arg(long, default_value = "desktop")]
        viewport: String,
        /// Instrucción visual opcional
        #[arg(long)]
        prompt: Option<String>,
    },

    /// [Cara 1] Inicia la Terminal Interactiva (TUI / REPL con soporte `@agente`, `/vision`, `/rag`)
    Chat,

    /// [Cara 2] Inicia el Servidor MCP (Model Context Protocol JSON-RPC 2.0 sobre `stdio`) para Antigravity IDE
    Mcp,

    /// [Cara 3] Inicia el Servidor ACP (Agent Client Protocol JSON-RPC 2.0 sobre `stdio`) para Neovim (`codecompanion.nvim`)
    Acp,

    /// [Cara 4] Inicia el Servidor Proxy HTTP OpenAI-Compatible (`http://<host>:<port>/v1`) para Neovim, Continue u OpenWebUI
    Serve {
        /// Dirección IP de escucha (default: 127.0.0.1; usa 0.0.0.0 para exponer en LAN)
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Puerto TCP local (default: 8090)
        #[arg(short, long, default_value_t = 8090)]
        port: u16,
    },

    /// [Cowork Mobile] Lanza herramientas de coworking: relay, mcp, acp, ttyd
    Cowork {
        /// Subcommand: relay, mcp, acp, ttyd (default: relay)
        #[command(subcommand)]
        subcommand: Option<CoworkSubcommand>,

        /// Puerto del relay (default: 8080)
        #[arg(long, default_value = "8080")]
        port: u16,

        /// Token de autenticación para el relay (RELAY_TOKEN)
        #[arg(long)]
        token: Option<String>,
    },
}

#[derive(Subcommand)]
enum CoworkSubcommand {
    /// Lanza el relay SSE (mcp-sse-relay)
    Relay,

    /// Lanza el servidor MCP (mcp_server.rs --sse)
    Mcp,

    /// Lanza el servidor ACP (acp_server.rs --sse)
    Acp,

    /// Lanza ttyd terminal web (ttyd -b 0.0.0.0 -p 7681 "lh.exe mcp")
    Ttyd,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let workspace = cli
        .workspace
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut config = HarnessConfig::load_for_workspace(cli.config.as_deref(), Some(&workspace))?;
    if let Some(srv) = cli.server.as_deref() {
        config.apply_server_override(srv);
    }

    match cli.command {
        Commands::Agents => {
            println!("================================================================================");
            println!("⚡ OPENED-AI: Catálogo de Agentes Configurados (config/config.yaml)");
            println!("================================================================================");
            let mut names: Vec<&String> = config.agents.keys().collect();
            names.sort();
            for name in names {
                if let Some(a) = config.agents.get(name) {
                    let ctx_mode = if a.disposable_context {
                        if a.auto_compact_context.unwrap_or(false) {
                            "🗑️  Contexto Efímero + 🗜️ Auto-Compactación Multi-Iteración"
                        } else {
                            "🗑️  Contexto Efímero/Desechable (KV se limpia al terminar)"
                        }
                    } else {
                        "🧠 Contexto Persistente + Slot Caching"
                    };
                    let reasoning_info = match a.reasoning_budget {
                        Some(rb) => format!(" | Reasoning Budget: {} tk", rb),
                        None => String::new(),
                    };
                    println!("• {:<20} | Rol: {}", name, a.role);
                    println!("  ├─ Modelo:          {}", a.model);
                    println!("  ├─ Modo KV Cache:   {}", ctx_mode);
                    println!(
                        "  ├─ Límite Pasos:    {} (Max Tokens/turno: {}{})",
                        a.max_iterations, a.max_tokens, reasoning_info
                    );
                    println!("  └─ Herramientas:    [{}]\n", a.allowed_tools.join(", "));
                }
            }
        }
        Commands::Skills => {
            println!("================================================================================");
            println!("📚 OPENED-AI: Catálogo de Skills de Carga Perezosa (Lazy-Loaded SKILL.md)");
            println!("================================================================================");
            let skills = discover_skills(&workspace);
            for s in skills {
                println!("• {:<24} | {}", s.name, s.description);
                if !s.triggers.is_empty() {
                    println!("  ├─ Triggers: {}", s.triggers.join(", "));
                }
                println!("  └─ Archivo:  {}\n", s.file_path.display());
            }
        }
        Commands::Status => {
            if let Some(ref cfg_path) = config.loaded_from {
                println!("Configuración cargada desde: {}", cfg_path.display());
            }
            println!("Conectando con router llama-swap en {} ...", config.server.base_url);
            let client = LlamaClient::new(config.server.clone())?;
            match client.list_models().await {
                Ok(models) => {
                    println!("[OK] Router llama-swap ACTIVO. Modelos disponibles ({}):", models.len());
                    for m in models {
                        println!("  - {}", m);
                    }
                }
                Err(e) => {
                    println!(
                        "[WARN] El router llama-swap no está corriendo actualmente en {}: {}",
                        config.server.base_url, e
                    );
                }
            }
        }
        Commands::Run { goal } => {
            let client = LlamaClient::new(config.server.clone())?;
            let summary = run_orchestrator(&client, &config, &workspace, &goal).await?;
            println!("\n📋 RESPUESTA FINAL DEL TI MANAGER:\n{}", summary.final_response);
        }
        Commands::Subagent { agent_name, task } => {
            let client = LlamaClient::new(config.server.clone())?;
            let report =
                run_disposable_subagent(&client, &config, &workspace, &agent_name, &task, None)
                    .await?;
            println!("\n{}", report.to_compact_prompt_block());
        }
        Commands::WebRag { query_or_url } => {
            let args = if query_or_url.starts_with("http://") || query_or_url.starts_with("https://")
            {
                json!({ "url": query_or_url, "max_chars": 4000 })
            } else {
                json!({ "query": query_or_url, "max_chars": 4000 })
            };
            let out = tool_web_rag(&args).await;
            println!("{}", out);
        }
        Commands::Vision {
            target,
            viewport,
            prompt,
        } => {
            let mut args = json!({
                "target": target,
                "viewport": viewport,
            });
            if let Some(p) = prompt {
                args["prompt"] = json!(p);
            }
            let out = tool_vision_inspect(&args, &workspace, &config.server.base_url).await;
            println!("{}", out);
        }
        Commands::Chat => {
            run_interactive_chat(&config, &workspace).await?;
        }
        Commands::Mcp => {
            run_mcp_stdio_server(&config, &workspace).await?;
        }
        Commands::Acp => {
            run_acp_stdio_server(&config, &workspace).await?;
        }
        Commands::Serve { host, port } => {
            run_openai_proxy_server(&config, &workspace, &host, port).await?;
        }
        Commands::Cowork { subcommand, port, token } => {
            let relay_dir = workspace.join("cowork_mobile").join("relay");
            let port = port.to_string();
            let token = token.unwrap_or_default();

            match subcommand {
                Some(CoworkSubcommand::Relay) | None => {
                    println!("🚀 Lanzando relay SSE desde: {}", relay_dir.display());
                    println!("   Puerto: {}, Token: {}", port, if token.is_empty() { "(no set)" } else { "****" });
                    let mut cmd = std::process::Command::new("cargo");
                    cmd.arg("run").arg("--release").arg("--bin").arg("mcp-sse-relay");
                    cmd.current_dir(&relay_dir);
                    cmd.env("PORT", &port);
                    if !token.is_empty() {
                        cmd.env("RELAY_TOKEN", &token);
                    }
                    let status = cmd.status().expect("Failed to launch relay");
                    if !status.success() {
                        eprintln!("❌ Relay exited with status: {}", status);
                        std::process::exit(1);
                    }
                }
                Some(CoworkSubcommand::Mcp) => {
                    println!("🚀 Lanzando MCP server desde: {}", relay_dir.display());
                    let mut cmd = std::process::Command::new("cargo");
                    cmd.arg("run").arg("--release").arg("--").arg("--sse");
                    cmd.current_dir(&relay_dir);
                    cmd.env("PORT", &port);
                    if !token.is_empty() {
                        cmd.env("RELAY_TOKEN", &token);
                    }
                    let status = cmd.status().expect("Failed to launch MCP server");
                    if !status.success() {
                        eprintln!("❌ MCP server exited with status: {}", status);
                        std::process::exit(1);
                    }
                }
                Some(CoworkSubcommand::Acp) => {
                    println!("🚀 Lanzando ACP server desde: {}", relay_dir.display());
                    let mut cmd = std::process::Command::new("cargo");
                    cmd.arg("run").arg("--release").arg("--").arg("--sse");
                    cmd.current_dir(&relay_dir);
                    cmd.env("PORT", &port);
                    if !token.is_empty() {
                        cmd.env("RELAY_TOKEN", &token);
                    }
                    let status = cmd.status().expect("Failed to launch ACP server");
                    if !status.success() {
                        eprintln!("❌ ACP server exited with status: {}", status);
                        std::process::exit(1);
                    }
                }
                Some(CoworkSubcommand::Ttyd) => {
                    println!("🚀 Lanzando ttyd terminal web (puerto 7681)");
                    let mut cmd = std::process::Command::new("ttyd");
                    cmd.arg("-b").arg("0.0.0.0").arg("-p").arg("7681").arg("lh.exe").arg("mcp");
                    let status = cmd.status().expect("Failed to launch ttyd");
                    if !status.success() {
                        eprintln!("❌ ttyd exited with status: {}", status);
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    Ok(())
}

