use crate::config::HarnessConfig;
use crate::core::checkpoint::{create_git_checkpoint, get_git_diff_summary};
use crate::core::client::{ChatMessage, LlamaClient};
use crate::core::loop_guard::{extract_tolerant_tool_calls, LoopGuard};
use crate::interfaces::tui_hud;
use crate::skills::match_triggered_skills;
use crate::tools::{execute_local_tool, get_tools_schema, runtime_os_shell_hint};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::time::Instant;

/// Reporte ultra-compacto (~150-250 tokens) que un subagente devuelve al TI Manager
/// después de que todo su contexto efímero y KV Cache son destruidos.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentReport {
    pub subagent_name: String,
    pub role: String,
    pub model_used: String,
    pub status: String,
    pub iterations_used: usize,
    pub ephemeral_tokens_discarded: usize,
    pub avg_tokens_per_sec: f64,
    pub git_checkpoint_ref: Option<String>,
    pub git_changes: String,
    pub summary_deliverable: String,
}

impl SubagentReport {
    /// Convierte el reporte a un bloque compacto para inyectar en el contexto del TI Manager
    pub fn to_compact_prompt_block(&self) -> String {
        format!(
            "[REPORTE SUBAGENTE `{}` ({}) | Estado: {} | Pasos: {} | KV Efímero Descartado: {} tokens @ {:.1} t/s]\n\
             Cambios Git:\n{}\n\
             Entregable:\n{}",
            self.subagent_name,
            self.role,
            self.status,
            self.iterations_used,
            self.ephemeral_tokens_discarded,
            self.avg_tokens_per_sec,
            self.git_changes,
            self.summary_deliverable
        )
    }
}

/// Ejecuta un Subagente con Contexto Efímero/Desechable.
/// - Crea una ventana `messages` completamente aislada.
/// - Al finalizar, `messages` sale de ámbito (Drop) liberando el 100% del historial intermedio.
pub async fn run_disposable_subagent(
    client: &LlamaClient,
    config: &HarnessConfig,
    workspace: &Path,
    agent_name: &str,
    task_brief: &str,
    context_hints: Option<&str>,
) -> Result<SubagentReport> {
    let agent_cfg = config
        .agents
        .get(agent_name)
        .with_context(|| format!("El subagente '{}' no existe en config.yaml", agent_name))?;

    let t_sub = Instant::now();
    tui_hud::push_action(
        0,
        "Agent",
        &format!("@{} ({})", agent_name, agent_cfg.model),
        task_brief.chars().take(64).collect::<String>().as_str(),
    );
    tui_hud::set_activity(format!("🤖 @{} (0/{}) • {}", agent_name, agent_cfg.max_iterations, agent_cfg.model));

    // 1. Si el subagente tiene permisos de escritura, creamos un Git Checkpoint automático
    let writes_files = agent_cfg
        .allowed_tools
        .iter()
        .any(|t| t == "apply_diff" || t == "write_file" || t == "shell_exec");

    let checkpoint_ref = if writes_files && config.context_budget.auto_git_checkpoint {
        create_git_checkpoint(workspace, &format!("pre-subagent-{}", agent_name)).ok().flatten()
    } else {
        None
    };

    // 2. Contexto Efímero Aislado (0 basura del TI Manager)
    let mut ephemeral_messages: Vec<ChatMessage> = Vec::new();
    ephemeral_messages.push(ChatMessage::system(&agent_cfg.system_prompt));

    let auto_skills_block = if let Some((matched, body)) = match_triggered_skills(workspace, task_brief, 2) {
        tui_hud::push_notice(1, false, &format!("📚 skills · {}", matched.join(", ")));
        body
    } else {
        String::new()
    };

    let user_payload = match context_hints {
        Some(hints) if !hints.trim().is_empty() => {
            format!(
                "Directorio de trabajo: {}\n{}\n{}\n\nContexto adicional proporcionado por el TI Manager:\n{}\n\nTarea Asignada:\n{}",
                workspace.display(),
                runtime_os_shell_hint(),
                auto_skills_block,
                hints,
                task_brief
            )
        }
        _ => format!(
            "Directorio de trabajo: {}\n{}\n{}\n\nTarea Asignada:\n{}",
            workspace.display(),
            runtime_os_shell_hint(),
            auto_skills_block,
            task_brief
        ),
    };
    ephemeral_messages.push(ChatMessage::user(user_payload));

    let tools_schema = get_tools_schema(&agent_cfg.allowed_tools);
    let mut loop_guard = LoopGuard::new(2);
    let mut total_ephemeral_tokens = 0usize;
    let mut speed_samples: Vec<f64> = Vec::new();
    let mut final_deliverable = String::new();
    let mut status = "COMPLETED".to_string();
    let mut steps_used = 0usize;

    for step in 1..=agent_cfg.max_iterations {
        steps_used = step;
        tui_hud::set_activity(format!(
            "🤖 @{} (Paso {}/{}) • {}",
            agent_name, step, agent_cfg.max_iterations, agent_cfg.model
        ));
        let resp = client
            .chat_completion(
                &agent_cfg.model,
                &ephemeral_messages,
                Some(tools_schema.clone()),
                agent_cfg.temperature,
                agent_cfg.max_tokens,
                config.context_budget.strip_historical_thinking,
            )
            .await?;

        total_ephemeral_tokens = resp.prompt_tokens + resp.completion_tokens;
        if resp.tokens_per_sec > 0.0 {
            speed_samples.push(resp.tokens_per_sec);
            tui_hud::set_speed(resp.tokens_per_sec);
        }

        if !resp.reasoning.trim().is_empty() {
            tui_hud::push_thinking(1, agent_name, &resp.reasoning);
        }

        let tool_calls = extract_tolerant_tool_calls(&resp.content, &resp.tool_calls);

        if tool_calls.is_empty() {
            // El subagente concluyó su trabajo y emitió su respuesta final
            final_deliverable = resp.content.trim().to_string();
            let avg = if speed_samples.is_empty() {
                resp.tokens_per_sec
            } else {
                speed_samples.iter().sum::<f64>() / speed_samples.len() as f64
            };
            tui_hud::push_group_footer(
                1,
                "✅",
                &format!(
                    "@{} · {} pasos · {:.1}s · {:.1} t/s · +{} tk ahorrados",
                    agent_name,
                    step,
                    t_sub.elapsed().as_secs_f64(),
                    avg,
                    total_ephemeral_tokens
                ),
            );
            break;
        }

        // Registrar el turno del asistente con sus tool_calls
        ephemeral_messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: if resp.content.is_empty() {
                None
            } else {
                Some(resp.content.clone())
            },
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
            name: None,
        });

        // Ejecutar cada herramienta solicitada dentro del contexto efímero
        for tc in tool_calls {
            let (verb, target) = tui_hud::format_tool_target(&tc.function.name, &tc.function.arguments);
            tui_hud::set_activity(format!(
                "🤖 @{} ({}/{}) • {} {}",
                agent_name, step, agent_cfg.max_iterations, verb, target
            ));
            tui_hud::push_action(
                1,
                verb,
                &target,
                &format!("paso {}/{}", step, agent_cfg.max_iterations),
            );

            let tool_output = match loop_guard.check_call(&tc.function.name, &tc.function.arguments)
            {
                Ok(()) => {
                    execute_local_tool(
                        &tc.function.name,
                        &tc.function.arguments,
                        workspace,
                        config.context_budget.max_shell_output_chars,
                        &config.server.base_url,
                    )
                    .await
                }
                Err(guard_msg) => {
                    tui_hud::push_notice(1, true, &format!("⚠️ {}", guard_msg));
                    guard_msg
                }
            };

            ephemeral_messages.push(ChatMessage::tool_result(
                &tc.id,
                &tc.function.name,
                tool_output,
            ));
        }

        // Auto-compactación protegiendo tamaño de argumentos (evita retener payloads write_file de 14 KB)
        if agent_cfg.auto_compact_context.unwrap_or(false) {
            let threshold = agent_cfg.compact_threshold_tokens.unwrap_or(16384);
            if resp.prompt_tokens >= threshold && ephemeral_messages.len() >= 6 {
                let before_len = ephemeral_messages.len();
                compact_ephemeral_history(&mut ephemeral_messages);
                tui_hud::push_notice(
                    1,
                    false,
                    &format!(
                        "· Compactación @{} ({} tk >= {}): {} → {} mensajes",
                        agent_name,
                        resp.prompt_tokens,
                        threshold,
                        before_len,
                        ephemeral_messages.len()
                    ),
                );
            }
        }

        if step == agent_cfg.max_iterations {
            status = "MAX_ITERATIONS_REACHED".to_string();
            final_deliverable = format!(
                "El subagente alcanzó el límite de {} iteraciones. Última respuesta parcial:\n{}",
                agent_cfg.max_iterations, resp.content
            );
            let avg = if speed_samples.is_empty() {
                0.0
            } else {
                speed_samples.iter().sum::<f64>() / speed_samples.len() as f64
            };
            tui_hud::push_group_footer(
                1,
                "!",
                &format!(
                    "@{} alcanzó límite ({} pasos · {:.1}s · {:.1} t/s · {} tk liberados)",
                    agent_name,
                    agent_cfg.max_iterations,
                    t_sub.elapsed().as_secs_f64(),
                    avg,
                    total_ephemeral_tokens
                ),
            );
        }
    }

    let avg_tps = if speed_samples.is_empty() {
        0.0
    } else {
        speed_samples.iter().sum::<f64>() / (speed_samples.len() as f64)
    };

    // Recortar el entregable según max_report_chars (por defecto 1600 chars, o hasta 6000+ en security_auditor)
    let max_report_chars = agent_cfg.max_report_chars.unwrap_or(1600);
    if final_deliverable.len() > max_report_chars {
        final_deliverable = format!(
            "{}...\n[Resumen truncado a {} chars para proteger la KV Cache del TI Manager]",
            &final_deliverable[..max_report_chars],
            max_report_chars
        );
    }

    let git_changes = if writes_files {
        get_git_diff_summary(workspace)
    } else {
        "N/A (Agente de solo lectura)".to_string()
    };

    // Al salir de esta función, `ephemeral_messages` se destruye de la memoria (Drop)
    Ok(SubagentReport {
        subagent_name: agent_name.to_string(),
        role: agent_cfg.role.clone(),
        model_used: agent_cfg.model.clone(),
        status,
        iterations_used: steps_used,
        ephemeral_tokens_discarded: total_ephemeral_tokens,
        avg_tokens_per_sec: avg_tps,
        git_checkpoint_ref: checkpoint_ref,
        git_changes,
        summary_deliverable: final_deliverable,
    })
}

fn summarize_tool_args_for_ledger(tool_name: &str, raw_args: &str) -> String {
    let args: Value = serde_json::from_str(raw_args).unwrap_or(Value::Null);
    match tool_name {
        "write_file" => {
            let path = args["path"].as_str().unwrap_or("?");
            let bytes = args["content"].as_str().map(|s| s.len()).unwrap_or(0);
            format!("{{\"path\":\"{}\", \"content_bytes\":{}}}", path, bytes)
        }
        "apply_diff" => {
            let path = args["path"].as_str().unwrap_or("?");
            let s_len = args["search"].as_str().map(|s| s.len()).unwrap_or(0);
            let r_len = args["replace"].as_str().map(|s| s.len()).unwrap_or(0);
            format!(
                "{{\"path\":\"{}\", \"search_chars\":{}, \"replace_chars\":{}}}",
                path, s_len, r_len
            )
        }
        "update_plan" => {
            let len = args["plan_markdown"].as_str().map(|s| s.len()).unwrap_or(0);
            format!("{{\"plan_bytes\":{}}}", len)
        }
        _ => {
            let clean = raw_args.replace(['\r', '\n'], " ");
            if clean.chars().count() <= 200 {
                clean
            } else {
                let head: String = clean.chars().take(196).collect();
                format!("{}...", head)
            }
        }
    }
}

/// Compacta los mensajes intermedios de una sesión larga (subagente o TI Manager)
/// preservando intactos:
/// - `messages[0]`: System Prompt
/// - `messages[1]`: Tarea Inicial del Usuario/TI Manager
/// - Una bitácora sintetizada y acotada (nunca duplica payloads gigantes de `write_file`)
/// - El último intercambio completo (sin partir nunca un par `assistant` -> `tool`)
pub(crate) fn compact_ephemeral_history(messages: &mut Vec<ChatMessage>) {
    if messages.len() <= 5 {
        return;
    }

    // Buscar un punto de corte limpio para el tail (mantener último intercambio completo >= 2 mensajes)
    let mut end_compact_idx = messages.len().saturating_sub(2);
    while end_compact_idx > 2 && messages[end_compact_idx].role == "tool" {
        end_compact_idx -= 1;
    }
    if end_compact_idx <= 2 {
        return;
    }

    let mut ledger_lines: Vec<String> = Vec::new();

    for msg in &messages[2..end_compact_idx] {
        match msg.role.as_str() {
            "assistant" => {
                if let Some(tcs) = &msg.tool_calls {
                    for tc in tcs {
                        let summary_args =
                            summarize_tool_args_for_ledger(&tc.function.name, &tc.function.arguments);
                        ledger_lines.push(format!(
                            "- Tool `{}`: {}",
                            tc.function.name, summary_args
                        ));
                    }
                }
                if let Some(text) = &msg.content {
                    let clean = text.trim();
                    if !clean.is_empty() {
                        let snippet: String = clean.chars().take(240).collect();
                        ledger_lines.push(format!("- Nota del agente: {}", snippet));
                    }
                }
            }
            "tool" => {
                if let Some(text) = &msg.content {
                    let first_lines: String = text
                        .lines()
                        .take(3)
                        .collect::<Vec<_>>()
                        .join(" | ")
                        .chars()
                        .take(220)
                        .collect();
                    ledger_lines.push(format!(
                        "  ↳ Resultado `{}`: {}",
                        msg.name.as_deref().unwrap_or("tool"),
                        first_lines
                    ));
                }
            }
            "user" => {
                // Evitar re-anidar encabezados de compactaciones previas de forma infinita
                if let Some(text) = &msg.content {
                    for line in text.lines() {
                        if line.starts_with("- ") || line.starts_with("  ↳ ") {
                            ledger_lines.push(line.chars().take(240).collect());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Limitar la bitácora a las últimas 24 entradas para garantizar que la memoria compactada sea <1.5K tokens
    if ledger_lines.len() > 24 {
        let drop_n = ledger_lines.len() - 24;
        ledger_lines.drain(..drop_n);
    }

    let summary_msg = ChatMessage::user(format!(
        "[MEMORIA COMPACTADA DE ITERACIONES PREVIAS — NO REPETIR ESTAS CONSULTAS]\n\
         {}\n\
         [Continúa con la tarea usando `apply_diff` para editar archivos existentes sin reescribir ficheros completos]",
        ledger_lines.join("\n")
    ));

    let tail: Vec<ChatMessage> = messages.split_off(end_compact_idx);
    messages.truncate(2);
    messages.push(summary_msg);
    messages.extend(tail);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::client::{FunctionCall, ToolCall};

    #[test]
    fn compact_history_shrinks_large_write_file_and_keeps_valid_tail() {
        let big_code = "fn main() { println!(\"hello\"); }\n".repeat(450); // ~14 KB
        let make_write = |id: &str| ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: id.to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({
                        "path": "cowork_mobile/relay/src/main.rs",
                        "content": big_code
                    })
                    .to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        };

        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("task"),
            make_write("c1"),
            ChatMessage::tool_result("c1", "write_file", "[OK] 14KB"),
            make_write("c2"),
            ChatMessage::tool_result("c2", "write_file", "[OK] 14KB"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "c3".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "shell_exec".to_string(),
                        arguments: r#"{"command":"cargo check"}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
                name: None,
            },
            ChatMessage::tool_result("c3", "shell_exec", "exit 0"),
        ];

        compact_ephemeral_history(&mut msgs);

        assert_eq!(msgs.len(), 5); // sys + user + compacted + c3 assistant + c3 tool
        assert_eq!(msgs[2].role, "user");
        assert!(msgs[2].content.as_deref().unwrap().len() < 1500);
        assert!(msgs[2].content.as_deref().unwrap().contains("content_bytes"));
        assert_eq!(msgs[3].role, "assistant");
        assert_eq!(msgs[4].role, "tool");
    }
}


