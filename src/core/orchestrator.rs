use crate::config::HarnessConfig;
use crate::core::client::{
    parse_valid_tool_args, sanitize_tool_calls_for_storage, ChatMessage, LlamaClient,
};
use crate::core::loop_guard::{extract_tolerant_tool_calls, LoopGuard};
use crate::core::subagent::{compact_ephemeral_history, run_disposable_subagent};
use crate::interfaces::tui_hud;
use crate::tools::{execute_local_tool, get_tools_schema, runtime_os_shell_hint};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

const MANAGER_SLOT_FILE: &str = "ti_manager_slot0.bin";
const SESSION_STATE_FILE: &str = ".harness/manager_session.json";
const STAGNATION_WARN_TURNS: usize = 5;
const STAGNATION_HARD_STOP: usize = 10;

pub struct OrchestratorSummary {
    pub final_response: String,
    pub manager_prompt_tokens: usize,
    pub total_ephemeral_tokens_saved: usize,
    pub subagents_invoked: usize,
}

/// Estado conversacional persistente del TI Manager (para mantener memoria entre turnos de `oai chat`
/// y permitir reanudar sesiones con `/resume` desde `.harness/manager_session.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorSession {
    pub messages: Vec<ChatMessage>,
}

impl OrchestratorSession {
    pub fn new(config: &HarnessConfig, workspace: &Path) -> Self {
        let system_prompt = build_manager_system_prompt(config, workspace);
        Self {
            messages: vec![ChatMessage::system(system_prompt)],
        }
    }

    pub fn session_path(workspace: &Path) -> PathBuf {
        workspace.join(SESSION_STATE_FILE)
    }

    pub fn load_saved(workspace: &Path) -> Option<Self> {
        let path = Self::session_path(workspace);
        let raw = fs::read_to_string(path).ok()?;
        let session: Self = serde_json::from_str(&raw).ok()?;
        if session.messages.is_empty() {
            None
        } else {
            Some(session)
        }
    }

    pub fn save(&self, workspace: &Path) {
        let path = Self::session_path(workspace);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json_str) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, json_str);
        }
    }

    pub fn clear_saved(workspace: &Path) {
        let _ = fs::remove_file(Self::session_path(workspace));
    }
}

fn build_manager_system_prompt(config: &HarnessConfig, workspace: &Path) -> String {
    let mgr_prompt = config
        .agents
        .get("ti_manager")
        .map(|a| a.system_prompt.trim())
        .unwrap_or("Eres el TI Manager de Opened-AI.");

    let mut subagent_roster =
        String::from("\nSubagentes disponibles en config.yaml para `delegate_subagent`:\n");
    let mut roster_names: Vec<&String> = config.agents.keys().collect();
    roster_names.sort();
    for name in roster_names {
        if name != "ti_manager" {
            if let Some(a) = config.agents.get(name) {
                subagent_roster.push_str(&format!(
                    "- `{}` ({}) [Modelo: {} | Tools: {}]\n",
                    name,
                    a.role,
                    a.model,
                    a.allowed_tools.join(", ")
                ));
            }
        }
    }

    let memory_path = workspace.join(".harness").join("MEMORY.md");
    let memory_block = fs::read_to_string(&memory_path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let truncated: String = s.chars().take(1200).collect();
            format!("\n[MEMORIA DEL PROYECTO (.harness/MEMORY.md)]:\n{}\n", truncated)
        })
        .unwrap_or_default();

    format!(
        "{}\n{}\nRegla anti-alucinación en `delegate_subagent`: NO afirmes de memoria nombres exactos de variantes, métodos o breaking changes de dependencias; delega el archivo, el error real observado y pide verificar la API instalada en disco cuando falte un símbolo. Para ejecutar builds, tests o solo reportar errores SIN editar archivos, delega SIEMPRE a `qa_tester` (nunca a `semi_sr_dev`). Delega subtareas atómicas: máximo 1 archivo objetivo (o ≤5 errores) por delegate_subagent.\nWorkspace base: {}\n{}{}",
        mgr_prompt,
        runtime_os_shell_hint(),
        workspace.display(),
        memory_block,
        subagent_roster
    )
}

/// Calcula y renderiza en consola una barra de avance leyendo checkboxes (`- [x]` / `- [ ]` o `✅` / `⬜`)
/// de `.harness/PLAN.md` para ofrecer puntos de control visibles en misiones largas.
pub fn format_plan_progress_bar(workspace: &Path) -> Option<String> {
    let plan_path = workspace.join(".harness").join("PLAN.md");
    let content = fs::read_to_string(plan_path).ok()?;

    let mut done = 0usize;
    let mut pending = 0usize;
    for line in content.lines() {
        let t = line.trim();
        let lower = t.to_lowercase();
        if lower.starts_with("- [x]") || t.contains("✅") {
            done += 1;
        } else if lower.starts_with("- [ ]") || t.contains("⬜") {
            pending += 1;
        }
    }

    let total = done + pending;
    if total == 0 {
        return None;
    }

    let pct = (done * 100) / total;
    let filled = (done * 16) / total;
    let bar = format!(
        "{}{}",
        "█".repeat(filled),
        "░".repeat(16usize.saturating_sub(filled))
    );
    Some(format!(
        "📊 [Checkpoint PLAN.md: [{}] {}% ({}/{} hitos completados)]",
        bar, pct, done, total
    ))
}

/// Modo de un solo tiro (`lh run`, `lh mcp`, `lh acp`, `lh serve`): crea sesión nueva.
pub async fn run_orchestrator(
    client: &LlamaClient,
    config: &HarnessConfig,
    workspace: &Path,
    user_goal: &str,
) -> Result<OrchestratorSummary> {
    let mut session = OrchestratorSession::new(config, workspace);
    run_orchestrator_with_session(&mut session, client, config, workspace, user_goal).await
}

/// Ejecuta el bucle principal del Agente Orquestador (`ti_manager`) reutilizando `session.messages`.
/// - Si `max_iterations == 0`, corre sin límite fijo de turnos protegido por:
///   1. Auto-compactación al superar `soft_limit_tokens`.
///   2. Barra de avance + checkpoints en `.harness/PLAN.md`.
///   3. Control anti-divagación (`STAGNATION_WARN_TURNS` / `STAGNATION_HARD_STOP`).
pub async fn run_orchestrator_with_session(
    session: &mut OrchestratorSession,
    client: &LlamaClient,
    config: &HarnessConfig,
    workspace: &Path,
    user_goal: &str,
) -> Result<OrchestratorSummary> {
    let mgr_cfg = config
        .agents
        .get("ti_manager")
        .context("El agente 'ti_manager' debe existir en config/config.yaml")?;

    if session.messages.is_empty() {
        *session = OrchestratorSession::new(config, workspace);
    }

    session.messages.push(ChatMessage::user(format!(
        "Workspace activo: {}\n\nObjetivo del Usuario:\n{}",
        workspace.display(),
        user_goal
    )));

    let tools_schema = get_tools_schema(&mgr_cfg.allowed_tools);
    let mut loop_guard = LoopGuard::new(2);
    let mut total_ephemeral_tokens_saved = 0usize;
    let mut subagents_invoked = 0usize;
    let mut manager_prompt_tokens = 0usize;
    let mut turns_since_progress = 0usize;
    let mut consecutive_subagent_stalls = 0usize;

    let unlimited = mgr_cfg.max_iterations == 0;
    let max_label = if unlimited {
        "∞ (Auto-Checkpoints + Anti-Divagación)".to_string()
    } else {
        mgr_cfg.max_iterations.to_string()
    };

    let t_mgr = Instant::now();
    let mut speed_samples: Vec<f64> = Vec::new();

    tui_hud::sync_plan(workspace);
    tui_hud::set_mode_orchestrator(&mgr_cfg.model, mgr_cfg.max_iterations);
    tui_hud::set_activity(format!("👔 TI Manager iniciando ({})", mgr_cfg.model));

    tui_hud::push_group_header(
        "👔",
        "TI MANAGER",
        &format!("{} · turnos {}", mgr_cfg.model, max_label),
    );

    let mut step = 0usize;
    loop {
        step += 1;
        if !unlimited && step > mgr_cfg.max_iterations {
            break;
        }

        let turn_tag = if unlimited {
            format!("{} (∞)", step)
        } else {
            format!("{}/{}", step, mgr_cfg.max_iterations)
        };
        tui_hud::set_activity(format!("👔 Pensando • Turno {}", turn_tag));

        let resp = client
            .chat_completion(
                &mgr_cfg.model,
                &session.messages,
                Some(tools_schema.clone()),
                mgr_cfg.temperature,
                mgr_cfg.max_tokens,
                config.context_budget.strip_historical_thinking,
            )
            .await?;

        manager_prompt_tokens = resp.prompt_tokens + resp.completion_tokens;
        if resp.tokens_per_sec > 0.0 {
            speed_samples.push(resp.tokens_per_sec);
        }
        tui_hud::set_kv_and_speed(manager_prompt_tokens, resp.tokens_per_sec);

        if !resp.reasoning.trim().is_empty() {
            tui_hud::push_thinking(0, &format!("TI Manager t{}", step), &resp.reasoning);
        }

        let tool_calls = extract_tolerant_tool_calls(&resp.content, &resp.tool_calls);

        if tool_calls.is_empty() {
            session.messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: Some(resp.content.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
            session.save(workspace);

            let avg_tps = if speed_samples.is_empty() {
                resp.tokens_per_sec
            } else {
                speed_samples.iter().sum::<f64>() / speed_samples.len() as f64
            };
            tui_hud::sync_plan(workspace);
            tui_hud::set_activity("Esperando instrucción");
            tui_hud::push_group_footer(
                0,
                "✅",
                &format!(
                    "Completado · {} turnos · {:.1}s · {:.1} t/s · {} subagentes · +{} tk ahorrados",
                    step,
                    t_mgr.elapsed().as_secs_f64(),
                    avg_tps,
                    subagents_invoked,
                    total_ephemeral_tokens_saved
                ),
            );
            return Ok(OrchestratorSummary {
                final_response: resp.content,
                manager_prompt_tokens,
                total_ephemeral_tokens_saved,
                subagents_invoked,
            });
        }

        let sanitized_calls = sanitize_tool_calls_for_storage(&tool_calls);
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: if resp.content.is_empty() {
                None
            } else {
                Some(resp.content.clone())
            },
            tool_calls: Some(sanitized_calls),
            tool_call_id: None,
            name: None,
        });

        let mut made_concrete_progress = false;

        for tc in tool_calls {
            let tool_name = tc.function.name.as_str();

            if let Err(err_msg) = parse_valid_tool_args(&tc.function.arguments) {
                tui_hud::push_notice(0, true, &format!("⚠️ Tool `{}` truncada/inválida; solicitando reintento limpio.", tool_name));
                session
                    .messages
                    .push(ChatMessage::tool_result(&tc.id, tool_name, err_msg));
                session.save(workspace);
                continue;
            }

            if let Err(guard_msg) = loop_guard.check_call(tool_name, &tc.function.arguments) {
                session
                    .messages
                    .push(ChatMessage::tool_result(&tc.id, tool_name, guard_msg));
                continue;
            }

            if tool_name == "delegate_subagent" {
                let args = parse_valid_tool_args(&tc.function.arguments).unwrap_or_default();
                let target_agent = args["agent_name"]
                    .as_str()
                    .or_else(|| args["subagent"].as_str())
                    .unwrap_or("semi_sr_dev");
                let task_brief = args["task_brief"]
                    .as_str()
                    .or_else(|| args["task"].as_str())
                    .or_else(|| args["prompt"].as_str())
                    .or_else(|| args["instructions"].as_str())
                    .or_else(|| args["description"].as_str())
                    .unwrap_or("");
                if task_brief.trim().is_empty() {
                    let msg = "[ERROR delegate_subagent]: falta `task_brief` no vacío. Indica en <150 palabras el archivo objetivo y los cambios exactos.".to_string();
                    session
                        .messages
                        .push(ChatMessage::tool_result(&tc.id, tool_name, msg));
                    session.save(workspace);
                    continue;
                }
                let context_hints = args["context_hints"].as_str();

                tui_hud::set_activity(format!("💾 Guardando slot KV ({})…", mgr_cfg.model));
                let _ = client.save_slot(&mgr_cfg.model, 0, MANAGER_SLOT_FILE).await;
                tui_hud::set_activity(format!("🤖 Delegando → @{}", target_agent));

                let (tool_result_str, subagent_stalled, stall_preview) =
                    match run_disposable_subagent(
                        client,
                        config,
                        workspace,
                        target_agent,
                        task_brief,
                        context_hints,
                    )
                    .await
                    {
                        Ok(report) => {
                            subagents_invoked += 1;
                            total_ephemeral_tokens_saved += report.ephemeral_tokens_discarded;
                            tui_hud::add_saved_kv(report.ephemeral_tokens_discarded, 1);
                            let completed = report.status == "COMPLETED";
                            if completed {
                                made_concrete_progress = true;
                            }
                            let preview: String =
                                report.summary_deliverable.chars().take(260).collect();
                            (report.to_compact_prompt_block(), !completed, preview)
                        }
                        Err(e) => {
                            let err = format!(
                                "[ERROR ejecutando subagente '{}']: {}",
                                target_agent, e
                            );
                            (err.clone(), true, err)
                        }
                    };

                tui_hud::set_activity(format!("♻️ Restaurando slot KV ({})…", mgr_cfg.model));
                let _ = client.restore_slot(&mgr_cfg.model, 0, MANAGER_SLOT_FILE).await;
                tui_hud::sync_plan(workspace);

                session.messages.push(ChatMessage::tool_result(
                    &tc.id,
                    tool_name,
                    tool_result_str,
                ));

                if subagent_stalled {
                    consecutive_subagent_stalls += 1;
                    if consecutive_subagent_stalls >= 2 {
                        let question = format!(
                            "@{target_agent} falló o agotó iteraciones {consecutive_subagent_stalls} veces seguidas. Último bloque:\n{stall_preview}\n\n¿Cómo deseas proceder?"
                        );
                        let options = vec![
                            "Dar pista exacta o firma de API para desbloquear".to_string(),
                            "Saltar este submódulo y continuar con el siguiente de PLAN.md"
                                .to_string(),
                            "Revertir cambios recientes en este módulo".to_string(),
                            "Pausar aquí y revisar manualmente".to_string(),
                        ];
                        tui_hud::push_notice(
                            0,
                            true,
                            &format!(
                                "🙋 Escalando al usuario tras {} fallos consecutivos de subagente.",
                                consecutive_subagent_stalls
                            ),
                        );
                        let user_choice = tui_hud::request_clarification(&question, &options).await;
                        session.messages.push(ChatMessage::user(format!(
                            "[ESCALACIÓN HUMANA TRAS {} FALLOS CONSECUTIVOS DE SUBAGENTE]: {}",
                            consecutive_subagent_stalls, user_choice
                        )));
                        consecutive_subagent_stalls = 0;
                    }
                } else {
                    consecutive_subagent_stalls = 0;
                }
                session.save(workspace);
            } else {
                let (verb, target) = tui_hud::format_tool_target(tool_name, &tc.function.arguments);
                tui_hud::set_activity(format!("👔 {} {} (Turno {})", verb, target, turn_tag));
                tui_hud::push_action(0, verb, &target, &format!("turno {}", step));
                let out = execute_local_tool(
                    tool_name,
                    &tc.function.arguments,
                    workspace,
                    config.context_budget.max_shell_output_chars,
                    &config.server.base_url,
                )
                .await;
                if tool_name == "update_plan"
                    || tool_name == "git_checkpoint"
                    || tool_name == "clarify"
                {
                    made_concrete_progress = true;
                    tui_hud::sync_plan(workspace);
                }
                session
                    .messages
                    .push(ChatMessage::tool_result(&tc.id, tool_name, out));
            }
        }

        if made_concrete_progress {
            turns_since_progress = 0;
            session.save(workspace);
        } else {
            turns_since_progress += 1;
            if turns_since_progress == STAGNATION_WARN_TURNS {
                tui_hud::push_notice(
                    0,
                    true,
                    &format!(
                        "[pausa] Checkpoint Anti-Divagación ({} turnos sin avance concreto). Solicitando hito o síntesis...",
                        turns_since_progress
                    ),
                );
                session.messages.push(ChatMessage::user(format!(
                    "[CONTROL DE AVANCE / ANTI-DIVAGACIÓN]: Llevas {} turnos consecutivos sin completar un subagente ni actualizar `.harness/PLAN.md`. NO sigas leyendo archivos repetidamente ni divagando: (1) actualiza `.harness/PLAN.md` con checkboxes `- [x]` / `- [ ]`, (2) delega una subtarea pequeña y concreta, o (3) entrega al usuario el reporte final con lo completado y lo pendiente.",
                    turns_since_progress
                )));
            } else if turns_since_progress >= STAGNATION_HARD_STOP {
                let bar_note = format_plan_progress_bar(workspace).unwrap_or_default();
                session.save(workspace);
                tui_hud::sync_plan(workspace);
                tui_hud::set_activity("Pausa anti-divagación");
                let avg_tps = if speed_samples.is_empty() {
                    0.0
                } else {
                    speed_samples.iter().sum::<f64>() / speed_samples.len() as f64
                };
                tui_hud::push_group_footer(
                    0,
                    "!",
                    &format!(
                        "Pausa anti-divagación · {} turnos · {:.1}s · {:.1} t/s",
                        step,
                        t_mgr.elapsed().as_secs_f64(),
                        avg_tps
                    ),
                );
                return Ok(OrchestratorSummary {
                    final_response: format!(
                        "⏸️ Control anti-divagación activado tras {} turnos sin registrar un nuevo hito concreto.\n{}\nRevisa `.harness/PLAN.md` y envía una instrucción puntual para continuar desde este checkpoint.",
                        turns_since_progress, bar_note
                    ),
                    manager_prompt_tokens,
                    total_ephemeral_tokens_saved,
                    subagents_invoked,
                });
            }
        }

        // Auto-compactación del TI Manager si supera soft_limit_tokens
        let compact_enabled = mgr_cfg.auto_compact_context.unwrap_or(true);
        let threshold = mgr_cfg
            .compact_threshold_tokens
            .unwrap_or(config.context_budget.soft_limit_tokens);
        if compact_enabled && resp.prompt_tokens >= threshold && session.messages.len() >= 6 {
            let before_len = session.messages.len();
            compact_ephemeral_history(&mut session.messages);
            tui_hud::push_notice(
                0,
                false,
                &format!(
                    "· Auto-compactación TI Manager ({} tk >= {}): {} → {} mensajes",
                    resp.prompt_tokens,
                    threshold,
                    before_len,
                    session.messages.len()
                ),
            );
            session.save(workspace);
        }
    }

    session.save(workspace);
    Ok(OrchestratorSummary {
        final_response: "Se alcanzó el número máximo de iteraciones del TI Manager.".to_string(),
        manager_prompt_tokens,
        total_ephemeral_tokens_saved,
        subagents_invoked,
    })
}
