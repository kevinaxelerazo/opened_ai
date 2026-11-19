use crate::config::HarnessConfig;
use crate::core::checkpoint::{create_git_checkpoint, get_git_diff_summary};
use crate::core::client::{
    parse_valid_tool_args, sanitize_tool_calls_for_storage, ChatMessage, LlamaClient,
};
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
    let writes_files = agent_cfg
        .allowed_tools
        .iter()
        .any(|t| t == "write_file" || t == "apply_diff")
        && !brief_forbids_edits(task_brief);
    let mut steps_without_edit = 0usize;
    let mut api_hallucination: Option<(&'static EcosystemApiRule, usize)> = None;

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

        // Registrar el turno del asistente con sus tool_calls saneados
        let sanitized_calls = sanitize_tool_calls_for_storage(&tool_calls);
        ephemeral_messages.push(ChatMessage {
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

        // Ejecutar cada herramienta solicitada dentro del contexto efímero
        let mut made_edit_this_step = false;
        let mut pending_api_nudge: Option<(&'static EcosystemApiRule, usize)> = None;
        for tc in tool_calls {
            let tool_output = if let Err(err_msg) = parse_valid_tool_args(&tc.function.arguments) {
                tui_hud::push_notice(
                    1,
                    true,
                    &format!("⚠️ Tool `{}` truncada/inválida en @{}; reintentando.", tc.function.name, agent_name),
                );
                err_msg
            } else {
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

                if let Some((rule, _)) = api_hallucination {
                    if tool_verified_api_source(&tc.function.name, &tc.function.arguments, rule) {
                        api_hallucination = None;
                    }
                }

                let api_blocked = if tc.function.name == "apply_diff"
                    || tc.function.name == "write_file"
                {
                    api_hallucination.and_then(|(rule, count)| {
                        if count >= 2 {
                            Some(format!(
                                "[ERROR BLOQUEO ANTI-ALUCINACIÓN ({})]: Llevas {} fallos consecutivos por símbolos/APIs inexistentes sin inspeccionar la dependencia real. Ediciones bloqueadas hasta verificar la firma real en: {}",
                                rule.ecosystem, count, rule.source_hint
                            ))
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };

                if let Some(block_msg) = api_blocked {
                    tui_hud::push_notice(
                        1,
                        true,
                        &format!("⚠️ @{} bloqueado hasta verificar API real ({}).", agent_name, api_hallucination.map(|(r, _)| r.ecosystem).unwrap_or("API")),
                    );
                    block_msg
                } else {
                    match loop_guard.check_call(&tc.function.name, &tc.function.arguments) {
                        Ok(()) => {
                            let out = execute_local_tool(
                                &tc.function.name,
                                &tc.function.arguments,
                                workspace,
                                config.context_budget.max_shell_output_chars,
                                &config.server.base_url,
                            )
                            .await;
                            if tc.function.name == "shell_exec" {
                                if let Some(rule) = detect_api_hallucination(&out) {
                                    let next_count = match api_hallucination {
                                        Some((prev, c)) if prev.ecosystem == rule.ecosystem => c + 1,
                                        _ => 1,
                                    };
                                    api_hallucination = Some((rule, next_count));
                                    pending_api_nudge = Some((rule, next_count));
                                }
                            }
                            out
                        }
                        Err(guard_msg) => {
                            tui_hud::push_notice(1, true, &format!("⚠️ {}", guard_msg));
                            guard_msg
                        }
                    }
                }
            };

            if (tc.function.name == "apply_diff" || tc.function.name == "write_file")
                && !tool_output.starts_with("[ERROR")
            {
                made_edit_this_step = true;
            }

            ephemeral_messages.push(ChatMessage::tool_result(
                &tc.id,
                &tc.function.name,
                tool_output,
            ));
        }

        if let Some((rule, count)) = pending_api_nudge {
            tui_hud::push_notice(
                1,
                true,
                &format!(
                    "⚠️ Error de símbolo/API ({}) en @{} (intento {}); exigiendo verificación real.",
                    rule.ecosystem, agent_name, count
                ),
            );
            ephemeral_messages.push(ChatMessage::user(format!(
                "[CORRECTIVO ANTI-ALUCINACIÓN DE API ({}) — intento {}]: El compilador/runtime reportó un símbolo, variante, método o import inexistente. NO adivines nombres de memoria ni des por ciertas versiones asumidas en el prompt. Inspecciona primero la definición real instalada en disco ({}) con `read_file`, `search_code` o `shell_exec` antes de volver a editar.",
                rule.ecosystem, count, rule.source_hint
            )));
        }

        if writes_files {
            if made_edit_this_step {
                steps_without_edit = 0;
            } else {
                steps_without_edit += 1;
                if should_nudge_edit_stall(
                    writes_files,
                    steps_without_edit,
                    step,
                    agent_cfg.max_iterations,
                ) {
                    tui_hud::push_notice(
                        1,
                        true,
                        &format!(
                            "⚠️ @{} lleva {} pasos sin editar archivos; forzando acción concreta.",
                            agent_name, steps_without_edit
                        ),
                    );
                    ephemeral_messages.push(ChatMessage::user(format!(
                        "[CORRECTIVO INTERNO]: Llevas {} pasos consecutivos inspeccionando o ejecutando sin modificar archivos (paso {}/{}). Deja de explorar y aplica ya tu primer `apply_diff` o `write_file` concreto sobre el archivo objetivo usando rutas relativas al workspace.",
                        steps_without_edit, step, agent_cfg.max_iterations
                    )));
                }
            }
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
            tui_hud::set_activity(format!(
                "🤖 @{} (Síntesis de cierre) • {}",
                agent_name, agent_cfg.model
            ));
            tui_hud::push_notice(
                1,
                false,
                &format!(
                    "· @{} alcanzó {} pasos; sintetizando avances y pendientes para TI Manager…",
                    agent_name, agent_cfg.max_iterations
                ),
            );

            let synth_prompt = format!(
                "[CIERRE POR LÍMITE DE {} ITERACIONES]: Se agotaron tus pasos de herramientas. NO invoques más herramientas ni bloques <tool_call>. \
                 Redacta AHORA un resumen ejecutivo conciso (<250 palabras) para el TI Manager con:\n\
                 1. Qué cambios, archivos o verificaciones concretas lograste completar.\n\
                 2. Qué errores exactos (`archivo:línea` y causa) o subtareas quedaron pendientes.\n\
                 3. Cuál debe ser el siguiente paso puntual recomendado.",
                agent_cfg.max_iterations
            );
            ephemeral_messages.push(ChatMessage::user(synth_prompt));

            if let Ok(synth_resp) = client
                .chat_completion(
                    &agent_cfg.model,
                    &ephemeral_messages,
                    None,
                    agent_cfg.temperature,
                    1024,
                    config.context_budget.strip_historical_thinking,
                )
                .await
            {
                total_ephemeral_tokens = synth_resp.prompt_tokens + synth_resp.completion_tokens;
                if synth_resp.tokens_per_sec > 0.0 {
                    speed_samples.push(synth_resp.tokens_per_sec);
                    tui_hud::set_speed(synth_resp.tokens_per_sec);
                }
                if !synth_resp.reasoning.trim().is_empty() {
                    tui_hud::push_thinking(1, agent_name, &synth_resp.reasoning);
                }
                let clean_synth = synth_resp.content.trim();
                if !clean_synth.is_empty() {
                    final_deliverable = format!(
                        "[SÍNTESIS POST-LÍMITE ({} pasos)]:\n{}",
                        agent_cfg.max_iterations, clean_synth
                    );
                }
            }

            if final_deliverable.trim().is_empty() {
                final_deliverable = build_fallback_progress_digest(
                    &ephemeral_messages,
                    agent_cfg.max_iterations,
                    &resp.content,
                );
            }

            let avg = if speed_samples.is_empty() {
                0.0
            } else {
                speed_samples.iter().sum::<f64>() / speed_samples.len() as f64
            };
            tui_hud::push_group_footer(
                1,
                "!",
                &format!(
                    "@{} alcanzó límite ({} pasos + síntesis · {:.1}s · {:.1} t/s · {} tk liberados)",
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

    // Recortar el entregable según max_report_chars (1600 normal, 2400 si trae líneas de error)
    let max_report_chars = effective_max_report_chars(agent_cfg.max_report_chars, &final_deliverable);
    if final_deliverable.chars().count() > max_report_chars {
        let truncated: String = final_deliverable.chars().take(max_report_chars).collect();
        final_deliverable = format!(
            "{}...\n[Resumen truncado a {} chars para proteger la KV Cache del TI Manager]",
            truncated, max_report_chars
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

/// Síntesis determinista de respaldo si el turno forzado de resumen fallara o devolviera vacío.
fn build_fallback_progress_digest(
    messages: &[ChatMessage],
    max_iters: usize,
    last_partial: &str,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    for msg in messages.iter().skip(2) {
        match msg.role.as_str() {
            "assistant" => {
                if let Some(tcs) = &msg.tool_calls {
                    for tc in tcs {
                        let args =
                            summarize_tool_args_for_ledger(&tc.function.name, &tc.function.arguments);
                        lines.push(format!("- Tool `{}`: {}", tc.function.name, args));
                    }
                }
            }
            "tool" => {
                if let Some(text) = &msg.content {
                    let preview: String = text
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(" | ")
                        .chars()
                        .take(220)
                        .collect();
                    lines.push(format!(
                        "  ↳ `{}`: {}",
                        msg.name.as_deref().unwrap_or("tool"),
                        preview
                    ));
                }
            }
            _ => {}
        }
    }
    if lines.len() > 12 {
        let drop_n = lines.len() - 12;
        lines.drain(..drop_n);
    }
    let mut out = format!(
        "El subagente alcanzó el límite de {} iteraciones sin síntesis LLM.\nÚltimas acciones y resultados:\n{}",
        max_iters,
        lines.join("\n")
    );
    let partial = last_partial.trim();
    if !partial.is_empty() {
        out.push_str(&format!("\nÚltima nota parcial:\n{}", partial));
    }
    out
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

    #[test]
    fn fallback_progress_digest_summarizes_last_tools_when_limit_reached_with_empty_content() {
        let msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("fix errors"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "c1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "shell_exec".to_string(),
                        arguments: r#"{"command":"cargo check"}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
                name: None,
            },
            ChatMessage::tool_result(
                "c1",
                "shell_exec",
                "error[E0277]: IntoResponse not satisfied\n  --> src/routes/auth_routes.rs:72",
            ),
        ];

        let digest = build_fallback_progress_digest(&msgs, 18, "");
        assert!(digest.contains("18 iteraciones"));
        assert!(digest.contains("shell_exec"));
        assert!(digest.contains("E0277"));
    }

    #[test]
    fn edit_stall_nudge_triggers_every_eight_steps_for_writers() {
        assert!(!should_nudge_edit_stall(false, 8, 8, 18));
        assert!(!should_nudge_edit_stall(true, 7, 7, 18));
        assert!(should_nudge_edit_stall(true, 8, 8, 18));
        assert!(!should_nudge_edit_stall(true, 9, 9, 18));
        assert!(should_nudge_edit_stall(true, 16, 16, 18));
        assert!(!should_nudge_edit_stall(true, 18, 18, 18));
    }

    #[test]
    fn api_hallucination_detector_matches_multiple_ecosystems_and_verifies_sources() {
        let rust = detect_api_hallucination("error[E0599]: no variant named `EcdsaP256Sha256` found")
            .expect("rust");
        assert_eq!(rust.ecosystem, "Rust");
        assert!(tool_verified_api_source(
            "read_file",
            r#"{"path":"C:\\Users\\Axel\\.cargo\\registry\\src\\jsonwebtoken\\src\\algorithms.rs"}"#,
            rust
        ));
        assert!(tool_verified_api_source(
            "search_code",
            r#"{"query":"Algorithm","subdir":".cargo/registry/src"}"#,
            rust
        ));
        assert!(tool_verified_api_source(
            "grep_search",
            r#"{"query":"Algorithm","path":"Cargo.lock"}"#,
            rust
        ));
        assert!(!tool_verified_api_source(
            "apply_diff",
            r#"{"path":"api/src/auth/jwt.rs"}"#,
            rust
        ));

        let ts = detect_api_hallucination("error TS2305: Module '\"axios\"' has no exported member 'Foo'.")
            .expect("ts");
        assert_eq!(ts.ecosystem, "JS/TS");
        assert!(tool_verified_api_source(
            "read_file",
            r#"{"path":"node_modules/axios/index.d.ts"}"#,
            ts
        ));

        let py = detect_api_hallucination("ImportError: cannot import name 'Bar' from 'fastapi'")
            .expect("python");
        assert_eq!(py.ecosystem, "Python");
        assert!(tool_verified_api_source(
            "shell_exec",
            r#"{"command":"python -c \"import inspect, fastapi\""}"#,
            py
        ));

        let go = detect_api_hallucination("./main.go:14:2: undefined: chi.NewMux").expect("go");
        assert_eq!(go.ecosystem, "Go");
        assert!(tool_verified_api_source(
            "shell_exec",
            r#"{"command":"go doc github.com/go-chi/chi/v5"}"#,
            go
        ));

        let cs = detect_api_hallucination("error CS0117: 'JwtSecurityToken' does not contain a definition for 'Foo'")
            .expect("cs");
        assert_eq!(cs.ecosystem, "C#/.NET");

        let java = detect_api_hallucination("error: cannot find symbol\n  symbol:   method foo()").expect("java");
        assert_eq!(java.ecosystem, "Java");
    }

    #[test]
    fn brief_forbids_edits_and_error_report_budget_work() {
        assert!(brief_forbids_edits("Run cargo check. Do NOT fix anything yet."));
        assert!(brief_forbids_edits("Solo reporta los errores encontrados, no edites."));
        assert!(!brief_forbids_edits("Fix api/src/auth/jwt.rs using apply_diff."));
        assert_eq!(effective_max_report_chars(None, "Todo OK"), 1600);
        assert_eq!(
            effective_max_report_chars(None, "error[E0599]: no method named encode"),
            2400
        );
        assert_eq!(effective_max_report_chars(Some(6000), "error[E0599]"), 6000);
    }
}

/// Detecta si el `task_brief` ordena explícitamente solo diagnosticar/reportar sin editar archivos.
fn brief_forbids_edits(brief: &str) -> bool {
    let lower = brief.to_lowercase();
    [
        "do not fix",
        "don't fix",
        "do not edit",
        "don't edit",
        "do not modify",
        "only report",
        "just report",
        "solo reporta",
        "no edites",
        "no modifiques",
    ]
    .iter()
    .any(|pat| lower.contains(pat))
}

/// Amplía el presupuesto de resumen a 2400 chars cuando el reporte incluye diagnósticos de error.
fn effective_max_report_chars(configured: Option<usize>, deliverable: &str) -> usize {
    if let Some(limit) = configured {
        return limit;
    }
    let has_errors = deliverable.contains("error[")
        || deliverable.contains("FAILED")
        || deliverable.contains("Traceback")
        || deliverable.contains("Línea");
    if has_errors {
        2400
    } else {
        1600
    }
}

/// Regla declarativa para detectar errores por símbolos/APIs alucinados según ecosistema.
struct EcosystemApiRule {
    ecosystem: &'static str,
    error_patterns: &'static [&'static str],
    verify_markers: &'static [&'static str],
    source_hint: &'static str,
}

const API_HALLUCINATION_RULES: &[EcosystemApiRule] = &[
    EcosystemApiRule {
        ecosystem: "Rust",
        error_patterns: &[
            "no variant named",
            "no method named",
            "method not found in",
            "no associated item named",
            "unresolved import",
            "cannot find attribute",
            "error[e0599]",
            "error[e0432]",
            "error[e0433]",
        ],
        verify_markers: &[".cargo", "registry", "cargo.lock", "rustup doc"],
        source_hint: "Cargo.lock y %USERPROFILE%\\.cargo\\registry\\src (o ~/.cargo/registry/src)",
    },
    EcosystemApiRule {
        ecosystem: "JS/TS",
        error_patterns: &[
            "has no exported member",
            "does not exist on type",
            "cannot find module",
            "ts2305",
            "ts2339",
            "ts2307",
        ],
        verify_markers: &["node_modules", "package-lock.json", "pnpm-lock", "yarn.lock", ".d.ts"],
        source_hint: "package.json y node_modules/<paquete>/**/*.d.ts",
    },
    EcosystemApiRule {
        ecosystem: "Python",
        error_patterns: &[
            "cannot import name",
            "has no attribute",
            "modulenotfounderror",
        ],
        verify_markers: &["site-packages", "dist-packages", "inspect", "help(", "dir(", "pydoc"],
        source_hint: "site-packages del entorno o `python -c \"import inspect, <pkg>; ...\"`",
    },
    EcosystemApiRule {
        ecosystem: "Go",
        error_patterns: &["undefined:", "has no field or method", "cannot find package"],
        verify_markers: &["go doc", "gomodcache", "pkg/mod", "go.mod"],
        source_hint: "go.mod y `go doc <paquete>.<Simbolo>` (o GOMODCACHE)",
    },
    EcosystemApiRule {
        ecosystem: "C#/.NET",
        error_patterns: &[
            "cs0117",
            "cs0234",
            "cs0246",
            "cs1061",
            "does not contain a definition for",
        ],
        verify_markers: &[".nuget", "packages", ".csproj", "ilspy"],
        source_hint: "*.csproj y %USERPROFILE%\\.nuget\\packages (o ~/.nuget/packages)",
    },
    EcosystemApiRule {
        ecosystem: "Java",
        error_patterns: &["cannot find symbol", "package ", "does not exist"],
        verify_markers: &[".m2", ".gradle", "javap", "pom.xml", "build.gradle"],
        source_hint: "pom.xml / build.gradle y `javap` / ~/.m2 / ~/.gradle",
    },
];

/// Detecta si la salida de compilación/ejecución indica un símbolo o API alucinada.
fn detect_api_hallucination(output: &str) -> Option<&'static EcosystemApiRule> {
    let lower = output.to_lowercase();
    API_HALLUCINATION_RULES.iter().find(|rule| {
        if rule.ecosystem == "Java" {
            lower.contains("cannot find symbol")
                || (lower.contains("error: package ") && lower.contains("does not exist"))
        } else {
            rule.error_patterns.iter().any(|pat| lower.contains(pat))
        }
    })
}

/// Comprueba si la herramienta invocada consulta fuentes reales de dependencias del ecosistema.
fn tool_verified_api_source(tool_name: &str, args: &str, rule: &EcosystemApiRule) -> bool {
    if !matches!(
        tool_name,
        "read_file" | "find_files" | "search_code" | "grep_search" | "shell_exec"
    ) {
        return false;
    }
    let lower = args.to_lowercase();
    rule.verify_markers.iter().any(|m| lower.contains(m))
}

/// Determina si un subagente con permisos de escritura lleva demasiados pasos sin editar archivos.
fn should_nudge_edit_stall(
    writes_files: bool,
    steps_without_edit: usize,
    step: usize,
    max_iterations: usize,
) -> bool {
    writes_files
        && steps_without_edit >= 8
        && steps_without_edit % 8 == 0
        && step < max_iterations
}


