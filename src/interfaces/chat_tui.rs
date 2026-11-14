use crate::config::HarnessConfig;
use crate::core::client::{ChatMessage, LlamaClient};
use crate::core::loop_guard::{extract_tolerant_tool_calls, LoopGuard};
use crate::core::orchestrator::{
    format_plan_progress_bar, run_orchestrator_with_session, OrchestratorSession,
};
use crate::core::subagent::{compact_ephemeral_history, run_disposable_subagent};
use crate::interfaces::tui_hud::{self, HudGuard};
use crate::skills::{discover_skills, format_skills_catalog, match_triggered_skills};
use crate::tools::{execute_local_tool, get_tools_schema, runtime_os_shell_hint};
use crate::tools::vision_cdp::tool_vision_inspect;
use crate::tools::web_rag::tool_web_rag;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChatMode {
    Orchestrator,
    DirectModel(String),
}

struct AppCtx {
    cumulative_ephemeral_saved: usize,
    total_subagents_called: usize,
    session: OrchestratorSession,
    mode: ChatMode,
    direct_histories: HashMap<String, Vec<ChatMessage>>,
    default_mgr_model: String,
    default_mgr_max_iters: usize,
}

/// Cara 1: Terminal Interactiva con Alternate Screen, HUD fijo superior, viewport scrolleable,
/// `<think>` colapsado extensible con `Ctrl+O` y cambio de modelo al vuelo (`/model`).
pub async fn run_interactive_chat(config: &HarnessConfig, workspace: &Path) -> Result<()> {
    let client = LlamaClient::new(config.server.clone())?;
    let mgr_cfg = config.agents.get("ti_manager");
    let default_mgr_model = mgr_cfg
        .map(|a| a.model.clone())
        .unwrap_or_else(|| "qwen38-distill".to_string());
    let default_mgr_max_iters = mgr_cfg.map(|a| a.max_iterations).unwrap_or(0);

    let mut ctx = AppCtx {
        cumulative_ephemeral_saved: 0,
        total_subagents_called: 0,
        session: OrchestratorSession::new(config, workspace),
        mode: ChatMode::Orchestrator,
        direct_histories: HashMap::new(),
        default_mgr_model: default_mgr_model.clone(),
        default_mgr_max_iters,
    };

    let _hud_guard = HudGuard::enter(
        workspace,
        &config.server.base_url,
        &default_mgr_model,
        config.context_budget.max_context_tokens,
        config.context_budget.soft_limit_tokens,
        default_mgr_max_iters,
    );
    tui_hud::set_interactive_clarify_enabled(true);

    // Sondeo rápido inicial a llama-swap para iluminar el indicador del HUD y poblar `/model`
    refresh_model_picker_catalog(&client, config).await;

    print_welcome_card(config, workspace);

    // Si no estamos en TTY interactivo (ej. stdin redirigido), usar modo secuencial simple
    if !tui_hud::is_active() {
        return run_plain_repl(&client, config, workspace, &mut ctx).await;
    }

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if ev_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let mut input_chars: Vec<char> = Vec::new();
    let mut cursor: usize = 0;
    let mut prompt_history: Vec<String> = Vec::new();
    let mut hist_idx: Option<usize> = None;
    let mut picker_navigated = false;

    tui_hud::set_input("", 0, false);

    loop {
        let Some(ev) = ev_rx.recv().await else {
            break;
        };

        match ev {
            Event::Resize(_, _) => {
                tui_hud::redraw();
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if tui_hud::is_model_picker_open() && !key.modifiers.contains(KeyModifiers::ALT) {
                    match key.code {
                        KeyCode::Up => {
                            tui_hud::move_model_picker_selection(-1);
                            picker_navigated = true;
                            continue;
                        }
                        KeyCode::Down => {
                            tui_hud::move_model_picker_selection(1);
                            picker_navigated = true;
                            continue;
                        }
                        KeyCode::Tab => {
                            if let Some(completed) = tui_hud::autocomplete_model_picker(false) {
                                input_chars = completed.chars().collect();
                                cursor = input_chars.len();
                                picker_navigated = true;
                            }
                            continue;
                        }
                        KeyCode::BackTab => {
                            if let Some(completed) = tui_hud::autocomplete_model_picker(true) {
                                input_chars = completed.chars().collect();
                                cursor = input_chars.len();
                                picker_navigated = true;
                            }
                            continue;
                        }
                        KeyCode::Esc => {
                            input_chars.clear();
                            cursor = 0;
                            picker_navigated = false;
                            tui_hud::set_input("", 0, false);
                            continue;
                        }
                        _ => {}
                    }
                }

                if handle_global_nav_key(&key, false) {
                    continue;
                }

                // Ctrl+C o Ctrl+D en estado idle -> salir restaurando PowerShell
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
                {
                    break;
                }

                match key.code {
                    KeyCode::Enter => {
                        let was_picker_open = tui_hud::is_model_picker_open();
                        let picked_val = if was_picker_open {
                            tui_hud::selected_model_picker_value()
                        } else {
                            None
                        };
                        let raw: String = input_chars.iter().collect();
                        let mut trimmed = raw.trim().to_string();

                        if was_picker_open && trimmed == "/model" && !picker_navigated {
                            refresh_model_picker_catalog(&client, config).await;
                            input_chars = "/model ".chars().collect();
                            cursor = input_chars.len();
                            tui_hud::set_input("/model ", cursor, false);
                            continue;
                        }

                        if was_picker_open {
                            let arg = trimmed.strip_prefix("/model").unwrap_or("").trim();
                            if picker_navigated || !arg.is_empty() {
                                if let Some(sel) = picked_val {
                                    trimmed = format!("/model {}", sel);
                                }
                            }
                        }

                        input_chars.clear();
                        cursor = 0;
                        hist_idx = None;
                        picker_navigated = false;
                        tui_hud::set_input("", 0, false);

                        if trimmed.is_empty() {
                            continue;
                        }
                        if !trimmed.starts_with("/model ")
                            && prompt_history.last().map(|s| s.as_str()) != Some(&trimmed)
                        {
                            prompt_history.push(trimmed.clone());
                        }
                        if trimmed == "/exit"
                            || trimmed == "/quit"
                            || trimmed == "exit"
                            || trimmed == "quit"
                        {
                            break;
                        }

                        if !trimmed.starts_with("/model ") {
                            tui_hud::push_user_prompt(&trimmed);
                        }

                        if handle_local_command(&trimmed, &client, config, workspace, &mut ctx)
                            .await
                        {
                            continue;
                        }

                        // Ejecutar tarea LLM permitiendo seguir scrolleando, usar Ctrl+O, responder `clarify` o cancelar con Ctrl+C
                        tui_hud::set_input("", 0, true);
                        let mut cancelled = false;
                        let mut clarify_chars: Vec<char> = Vec::new();
                        let mut clarify_cursor: usize = 0;
                        let turn_started = Instant::now();
                        let mut ticker = tokio::time::interval(Duration::from_secs(1));
                        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        ticker.tick().await;
                        {
                            let fut = execute_prompt_turn(
                                &trimmed, &client, config, workspace, &mut ctx,
                            );
                            tokio::pin!(fut);

                            loop {
                                tokio::select! {
                                    _ = &mut fut => break,
                                    _ = ticker.tick() => {
                                        if !tui_hud::is_streaming() && !tui_hud::is_awaiting_clarify() {
                                            let elapsed = turn_started.elapsed().as_secs();
                                            let label = format_waiting_status(&client, elapsed).await;
                                            tui_hud::set_waiting_activity(label);
                                        }
                                    }
                                    maybe_ev = ev_rx.recv() => {
                                        match maybe_ev {
                                            Some(Event::Resize(_, _)) => tui_hud::redraw(),
                                            Some(Event::Key(k)) if k.kind != KeyEventKind::Release => {
                                                if k.modifiers.contains(KeyModifiers::CONTROL)
                                                    && k.code == KeyCode::Char('c')
                                                {
                                                    tui_hud::cancel_clarification();
                                                    cancelled = true;
                                                    break;
                                                }
                                                if tui_hud::is_awaiting_clarify() {
                                                    if k.code == KeyCode::Enter {
                                                        let ans: String = clarify_chars.drain(..).collect();
                                                        clarify_cursor = 0;
                                                        if !ans.trim().is_empty() {
                                                            tui_hud::submit_clarification(&ans);
                                                        }
                                                        continue;
                                                    }
                                                    if handle_line_edit_key(
                                                        &k,
                                                        &mut clarify_chars,
                                                        &mut clarify_cursor,
                                                        true,
                                                    ) {
                                                        continue;
                                                    }
                                                }
                                                let _ = handle_global_nav_key(&k, true);
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }

                        tui_hud::cancel_clarification();
                        if cancelled {
                            tui_hud::push_notice(
                                0,
                                true,
                                "⛔ Ejecución interrumpida (Ctrl+C). Puedes retomar con `/resume` o enviar otro prompt.",
                            );
                            restore_hud_idle(&ctx.mode);
                        }
                        tui_hud::set_input("", 0, false);
                    }
                    KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                        if !prompt_history.is_empty() {
                            let next = match hist_idx {
                                None => prompt_history.len() - 1,
                                Some(i) => i.saturating_sub(1),
                            };
                            hist_idx = Some(next);
                            input_chars = prompt_history[next].chars().collect();
                            cursor = input_chars.len();
                            let s: String = input_chars.iter().collect();
                            tui_hud::set_input(&s, cursor, false);
                        }
                    }
                    KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                        if let Some(i) = hist_idx {
                            if i + 1 < prompt_history.len() {
                                let next = i + 1;
                                hist_idx = Some(next);
                                input_chars = prompt_history[next].chars().collect();
                            } else {
                                hist_idx = None;
                                input_chars.clear();
                            }
                            cursor = input_chars.len();
                            let s: String = input_chars.iter().collect();
                            tui_hud::set_input(&s, cursor, false);
                        }
                    }
                    _ => {
                        let was_open = tui_hud::is_model_picker_open();
                        if handle_line_edit_key(&key, &mut input_chars, &mut cursor, false) {
                            picker_navigated = false;
                            if !was_open && tui_hud::is_model_picker_open() {
                                refresh_model_picker_catalog(&client, config).await;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Maneja la edición de línea en el footer (tanto en modo libre como al responder `clarify`).
fn handle_line_edit_key(
    key: &KeyEvent,
    buf: &mut Vec<char>,
    cursor: &mut usize,
    is_busy: bool,
) -> bool {
    match key.code {
        KeyCode::Backspace => {
            if *cursor > 0 {
                *cursor -= 1;
                buf.remove(*cursor);
                let s: String = buf.iter().collect();
                tui_hud::set_input(&s, *cursor, is_busy);
            }
            true
        }
        KeyCode::Delete => {
            if *cursor < buf.len() {
                buf.remove(*cursor);
                let s: String = buf.iter().collect();
                tui_hud::set_input(&s, *cursor, is_busy);
            }
            true
        }
        KeyCode::Left => {
            if *cursor > 0 {
                *cursor -= 1;
                let s: String = buf.iter().collect();
                tui_hud::set_input(&s, *cursor, is_busy);
            }
            true
        }
        KeyCode::Right => {
            if *cursor < buf.len() {
                *cursor += 1;
                let s: String = buf.iter().collect();
                tui_hud::set_input(&s, *cursor, is_busy);
            }
            true
        }
        KeyCode::Home if !is_busy && !key.modifiers.contains(KeyModifiers::CONTROL) => {
            *cursor = 0;
            let s: String = buf.iter().collect();
            tui_hud::set_input(&s, *cursor, is_busy);
            true
        }
        KeyCode::End if !is_busy && !key.modifiers.contains(KeyModifiers::CONTROL) => {
            *cursor = buf.len();
            let s: String = buf.iter().collect();
            tui_hud::set_input(&s, *cursor, is_busy);
            true
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            buf.clear();
            *cursor = 0;
            tui_hud::set_input("", 0, is_busy);
            true
        }
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            *cursor = 0;
            let s: String = buf.iter().collect();
            tui_hud::set_input(&s, *cursor, is_busy);
            true
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            *cursor = buf.len();
            let s: String = buf.iter().collect();
            tui_hud::set_input(&s, *cursor, is_busy);
            true
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            buf.insert(*cursor, c);
            *cursor += 1;
            let s: String = buf.iter().collect();
            tui_hud::set_input(&s, *cursor, is_busy);
            true
        }
        _ => false,
    }
}

/// Maneja teclas globales de navegación del viewport y `Ctrl+O` para expandir `<think>`.
fn handle_global_nav_key(key: &KeyEvent, is_busy: bool) -> bool {
    // Ctrl+O: alternar vista expandida de cadenas <think>
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('o') | KeyCode::Char('O'))
    {
        tui_hud::toggle_thinking();
        return true;
    }

    // Alt+Up / Alt+Down se reservan para historial de prompts cuando estamos idle
    if !is_busy && key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }

    match key.code {
        KeyCode::PageUp => {
            tui_hud::scroll_up(12);
            true
        }
        KeyCode::PageDown => {
            tui_hud::scroll_down(12);
            true
        }
        // Alternate Scroll (\x1b[?1007h) traduce rueda del ratón a Up / Down
        KeyCode::Up => {
            tui_hud::scroll_up(3);
            true
        }
        KeyCode::Down => {
            tui_hud::scroll_down(3);
            true
        }
        KeyCode::Home if is_busy || key.modifiers.contains(KeyModifiers::CONTROL) => {
            tui_hud::scroll_up(10_000);
            true
        }
        KeyCode::End if is_busy || key.modifiers.contains(KeyModifiers::CONTROL) => {
            tui_hud::scroll_to_bottom();
            true
        }
        _ => false,
    }
}

async fn run_plain_repl(
    client: &LlamaClient,
    config: &HarnessConfig,
    workspace: &Path,
    ctx: &mut AppCtx,
) -> Result<()> {
    let stdin = io::stdin();
    loop {
        print!("\n⚡ oai [{}]> ", workspace.display());
        io::stdout().flush()?;
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "/exit" || trimmed == "/quit" || trimmed == "exit" || trimmed == "quit" {
            break;
        }
        if handle_local_command(&trimmed, client, config, workspace, ctx).await {
            continue;
        }
        execute_prompt_turn(&trimmed, client, config, workspace, ctx).await;
    }
    Ok(())
}

async fn handle_local_command(
    input: &str,
    client: &LlamaClient,
    config: &HarnessConfig,
    workspace: &Path,
    ctx: &mut AppCtx,
) -> bool {
    if input == "/help" || input == "?" {
        print_help();
        return true;
    }

    if input == "/think" {
        let on = tui_hud::toggle_thinking();
        tui_hud::push_notice(
            0,
            false,
            if on {
                "💭 Cadenas <think> EXPANDIDAS (usa `Ctrl+O` o `/think` para colapsar)."
            } else {
                "💭 Cadenas <think> COLAPSADAS (usa `Ctrl+O` o `/think` para expandir)."
            },
        );
        return true;
    }

    if input == "/clear" {
        match &ctx.mode {
            ChatMode::Orchestrator => {
                ctx.cumulative_ephemeral_saved = 0;
                ctx.total_subagents_called = 0;
                ctx.session = OrchestratorSession::new(config, workspace);
                OrchestratorSession::clear_saved(workspace);
                tui_hud::clear_transcript();
                tui_hud::with_update(|s| {
                    s.kv_used = 0;
                    s.kv_saved = 0;
                    s.subagents_count = 0;
                    s.activity = "Sesión reiniciada".to_string();
                });
                tui_hud::push_notice(
                    0,
                    false,
                    "🧹 Memoria conversacional del TI Manager, sesión guardada y contadores reiniciados.",
                );
            }
            ChatMode::DirectModel(m) => {
                let name = m.clone();
                ctx.direct_histories.remove(&name);
                tui_hud::set_kv_and_speed(0, 0.0);
                tui_hud::set_activity(format!("Chat directo `{}` reiniciado", name));
                tui_hud::push_notice(
                    0,
                    false,
                    &format!(
                        "🧹 Historial de chat directo con `{}` reiniciado (TI Manager intacto).",
                        name
                    ),
                );
            }
        }
        return true;
    }

    if input == "/resume" {
        match OrchestratorSession::load_saved(workspace) {
            Some(loaded) => {
                let count = loaded.messages.len();
                ctx.session = loaded;
                ctx.mode = ChatMode::Orchestrator;
                tui_hud::set_mode_orchestrator(&ctx.default_mgr_model, ctx.default_mgr_max_iters);
                tui_hud::set_kv_budget(
                    config.context_budget.max_context_tokens,
                    config.context_budget.soft_limit_tokens,
                );
                tui_hud::sync_plan(workspace);
                let mut msg = format!(
                    "♻️ Sesión del TI Manager restaurada desde `.harness/manager_session.json` ({} mensajes).",
                    count
                );
                if let Some(bar) = format_plan_progress_bar(workspace) {
                    msg.push('\n');
                    msg.push_str(&bar);
                }
                tui_hud::push_notice(0, false, &msg);
            }
            None => {
                tui_hud::push_notice(
                    0,
                    true,
                    "ℹ️ No existe una sesión previa en `.harness/manager_session.json`.",
                );
            }
        }
        return true;
    }

    if input == "/model" || input.starts_with("/model ") {
        let arg = input.strip_prefix("/model").unwrap_or("").trim();
        let lower = arg.to_lowercase();
        if arg.is_empty()
            || lower == "manager"
            || lower == "auto"
            || lower == "orchestrator"
            || lower == "ti_manager"
        {
            handle_model_command(
                arg,
                client,
                config,
                &mut ctx.mode,
                &ctx.default_mgr_model,
                ctx.default_mgr_max_iters,
            )
            .await;
            return true;
        }
        // `/model <id>` se ejecuta dentro de `execute_prompt_turn` para permitir scroll,
        // `Ctrl+C` y monitoreo en vivo mientras llama-swap realiza el hot-swap del GGUF.
        return false;
    }

    if input == "/agents" {
        let mut names: Vec<&String> = config.agents.keys().collect();
        names.sort();
        let mut body = String::new();
        for n in names {
            if let Some(a) = config.agents.get(n) {
                body.push_str(&format!("• @{:<19} → {} [{}]\n", n, a.role, a.model));
            }
        }
        tui_hud::push_card(
            "📋",
            "AGENTES CONFIGURADOS",
            "config/config.yaml",
            body.trim_end(),
        );
        return true;
    }

    if input == "/skills" {
        let skills = discover_skills(workspace);
        let mut body = String::new();
        for s in skills {
            body.push_str(&format!(
                "• {:<26} [Triggers: {}]\n",
                s.name,
                s.triggers.join(", ")
            ));
        }
        tui_hud::push_card("📚", "SKILLS INSTALADAS", "Lazy-Loaded", body.trim_end());
        return true;
    }

    if input == "/plan" {
        tui_hud::sync_plan(workspace);
        let plan_path = workspace.join(".harness").join("PLAN.md");
        if let Ok(content) = fs::read_to_string(&plan_path) {
            let mut body = String::new();
            if let Some(bar) = format_plan_progress_bar(workspace) {
                body.push_str(&bar);
                body.push_str("\n\n");
            }
            body.push_str(&content);
            tui_hud::push_card("📊", "PROGRESO .harness/PLAN.md", "", body.trim_end());
        } else {
            tui_hud::push_notice(
                0,
                true,
                "ℹ️ Aún no existe `.harness/PLAN.md` en este workspace.",
            );
        }
        return true;
    }

    if input == "/status" {
        match client.list_models().await {
            Ok(models) => {
                tui_hud::set_server_status(true, models.len());
                tui_hud::push_notice(
                    0,
                    false,
                    &format!(
                        "✅ llama-swap ACTIVO en {}. Modelos ({}): {:?}",
                        config.server.base_url,
                        models.len(),
                        models
                    ),
                );
            }
            Err(e) => {
                tui_hud::set_server_status(false, 0);
                tui_hud::push_notice(
                    0,
                    true,
                    &format!(
                        "⚠️ llama-swap no responde en {}: {}",
                        config.server.base_url, e
                    ),
                );
            }
        }
        return true;
    }

    false
}

async fn execute_prompt_turn(
    input: &str,
    client: &LlamaClient,
    config: &HarnessConfig,
    workspace: &Path,
    ctx: &mut AppCtx,
) {
    if let Some(rest) = input.strip_prefix("/model ") {
        activate_direct_model_with_swap(rest.trim(), client, config, &mut ctx.mode).await;
        return;
    }

    if let Some(rest) = input.strip_prefix("/vision ") {
        let parts: Vec<&str> = rest.trim().splitn(2, ' ').collect();
        let target = parts[0];
        let prompt = parts.get(1).copied().unwrap_or(
            "Analiza visualmente esta interfaz, detecta fallos de responsiveness, contraste o layout.",
        );
        let args = json!({
            "target": target,
            "viewport": "desktop",
            "prompt": prompt
        });
        tui_hud::set_activity("👁️ Ejecutando /vision...");
        let out = tool_vision_inspect(&args, workspace, &config.server.base_url).await;
        restore_hud_idle(&ctx.mode);
        tui_hud::push_card("👁️", "VISION INSPECT", target, &out);
        return;
    }

    if let Some(rest) = input.strip_prefix("/rag ") {
        let q = rest.trim();
        let args = if q.starts_with("http://") || q.starts_with("https://") {
            json!({ "url": q, "max_chars": 4500 })
        } else {
            json!({ "query": q, "max_chars": 4500 })
        };
        tui_hud::set_activity("🌐 Ejecutando /rag...");
        let out = tool_web_rag(&args).await;
        restore_hud_idle(&ctx.mode);
        tui_hud::push_card("🌐", "WEB RAG", q, &out);
        return;
    }

    // Atajo `@nombre_agente <tarea>` o `/subagent <nombre> <tarea>`
    let direct_subagent = if let Some(rest) = input.strip_prefix('@') {
        let mut split = rest.splitn(2, char::is_whitespace);
        if let (Some(ag), Some(task)) = (split.next(), split.next()) {
            Some((ag.trim(), task.trim()))
        } else {
            None
        }
    } else if let Some(rest) = input.strip_prefix("/subagent ") {
        let mut split = rest.trim().splitn(2, char::is_whitespace);
        if let (Some(ag), Some(task)) = (split.next(), split.next()) {
            Some((ag.trim(), task.trim()))
        } else {
            None
        }
    } else {
        None
    };

    if let Some((agent_name, task_brief)) = direct_subagent {
        match run_disposable_subagent(client, config, workspace, agent_name, task_brief, None).await
        {
            Ok(rep) => {
                ctx.cumulative_ephemeral_saved += rep.ephemeral_tokens_discarded;
                ctx.total_subagents_called += 1;
                tui_hud::add_saved_kv(rep.ephemeral_tokens_discarded, 1);
                restore_hud_idle(&ctx.mode);
                tui_hud::push_card(
                    "🤖",
                    &format!("SUBAGENTE @{}", agent_name),
                    &format!("{} pasos · {:.1} t/s", rep.iterations_used, rep.avg_tokens_per_sec),
                    &rep.to_compact_prompt_block(),
                );
            }
            Err(e) => {
                restore_hud_idle(&ctx.mode);
                tui_hud::push_notice(
                    0,
                    true,
                    &format!("❌ Error ejecutando subagente `{}`: {}", agent_name, e),
                );
            }
        }
        return;
    }

    match &ctx.mode {
        ChatMode::Orchestrator => {
            match run_orchestrator_with_session(&mut ctx.session, client, config, workspace, input)
                .await
            {
                Ok(summary) => {
                    ctx.cumulative_ephemeral_saved += summary.total_ephemeral_tokens_saved;
                    ctx.total_subagents_called += summary.subagents_invoked;
                    tui_hud::push_card(
                        "📋",
                        "RESPUESTA DEL TI MANAGER",
                        &format!(
                            "KV: {} tk · Subagentes: {}",
                            summary.manager_prompt_tokens, summary.subagents_invoked
                        ),
                        &summary.final_response,
                    );
                }
                Err(e) => {
                    tui_hud::set_activity("⚠️ Error en TI Manager");
                    tui_hud::push_notice(0, true, &format!("❌ Error ejecutando TI Manager: {}", e));
                }
            }
        }
        ChatMode::DirectModel(model_id) => {
            run_direct_single_model_turn(
                client,
                config,
                workspace,
                model_id,
                input,
                &mut ctx.direct_histories,
            )
            .await;
        }
    }
}

async fn format_waiting_status(client: &LlamaClient, elapsed_secs: u64) -> String {
    if let Ok(running) = client.running_models().await {
        if let Some((model, state)) = running.iter().find(|(_, st)| st != "ready") {
            return format!("🔄 Swap llama-swap → {} [{}] • {}s", model, state, elapsed_secs);
        }
        if let Some((model, _)) = running.first() {
            return format!("⏳ Esperando {} • {}s", model, elapsed_secs);
        }
    }
    format!("⏳ Esperando respuesta de llama-swap • {}s", elapsed_secs)
}

async fn fetch_live_models(client: &LlamaClient) -> Option<Vec<String>> {
    match client.list_models().await {
        Ok(m) => {
            tui_hud::set_server_status(true, m.len());
            Some(m)
        }
        Err(_) => {
            tui_hud::set_server_status(false, 0);
            None
        }
    }
}

async fn refresh_model_picker_catalog(client: &LlamaClient, config: &HarnessConfig) {
    let models = match fetch_live_models(client).await {
        Some(m) => m,
        None => {
            let mut cfg_models = BTreeSet::new();
            for a in config.agents.values() {
                cfg_models.insert(a.model.clone());
            }
            cfg_models.into_iter().collect()
        }
    };
    let running = client.running_models().await.unwrap_or_default();
    tui_hud::set_model_picker_catalog(models, running);
}

async fn activate_direct_model_with_swap(
    arg: &str,
    client: &LlamaClient,
    config: &HarnessConfig,
    mode: &mut ChatMode,
) {
    let live_models = fetch_live_models(client).await;

    if let Some(ref models) = live_models {
        if !models.iter().any(|m| m == arg) {
            tui_hud::push_notice(
                0,
                true,
                &format!(
                    "⚠️ El modelo `{}` no aparece en llama-swap (`{:?}`). Usa `/model` para ver los IDs exactos.",
                    arg, models
                ),
            );
            restore_hud_idle(mode);
            return;
        }
    } else {
        tui_hud::push_notice(
            0,
            true,
            &format!(
                "⚠️ No se pudo verificar `{}` contra llama-swap (`{}`; offline); intentando seleccionar.",
                arg, config.server.base_url
            ),
        );
    }

    *mode = ChatMode::DirectModel(arg.to_string());
    tui_hud::set_mode_direct(arg);
    tui_hud::set_activity(format!("🔄 Precargando `{}` en llama-swap…", arg));
    tui_hud::push_notice(
        0,
        false,
        &format!(
            "⚡ Modo Directo activado (`{}`). Disparando hot-swap en llama-swap… (si carga desde HDD puede tardar ~1-2 min). Usa `/model manager` para volver.",
            arg
        ),
    );

    let t0 = Instant::now();
    match client.ensure_model_loaded(arg).await {
        Ok(loaded_fresh) => {
            let n_ctx = client
                .model_context_size(arg)
                .await
                .unwrap_or(config.context_budget.max_context_tokens);
            let direct_soft = effective_direct_soft_limit(n_ctx, config.context_budget.soft_limit_tokens);
            tui_hud::set_kv_budget(n_ctx, direct_soft);
            restore_hud_idle(mode);
            let status_msg = if loaded_fresh {
                format!(
                    "✅ `{}` cargado y listo en llama-swap ({:.1}s · n_ctx={} tk · soft={}). Ya puedes enviar tu prompt.",
                    arg,
                    t0.elapsed().as_secs_f64(),
                    n_ctx,
                    direct_soft
                )
            } else {
                format!(
                    "✅ `{}` ya estaba listo en llama-swap (n_ctx={} tk · soft={}).",
                    arg, n_ctx, direct_soft
                )
            };
            tui_hud::push_notice(0, false, &status_msg);
        }
        Err(e) => {
            tui_hud::set_activity(format!("⚠️ Error precargando `{}`", arg));
            tui_hud::push_notice(
                0,
                true,
                &format!(
                    "⚠️ Modo directo `{}` seleccionado, pero error al precargar en llama-swap: {}",
                    arg, e
                ),
            );
        }
    }
}

fn effective_direct_soft_limit(n_ctx: usize, default_soft: usize) -> usize {
    if n_ctx <= 4096 {
        return (n_ctx * 3) / 4;
    }
    let proportional = ((n_ctx * 3) / 4).min(n_ctx.saturating_sub(4096));
    if n_ctx >= default_soft {
        proportional.max(default_soft.min(n_ctx.saturating_sub(4096)))
    } else {
        proportional
    }
}

async fn handle_model_command(
    arg: &str,
    client: &LlamaClient,
    config: &HarnessConfig,
    mode: &mut ChatMode,
    default_mgr_model: &str,
    default_mgr_max_iters: usize,
) {
    let lower = arg.to_lowercase();
    if lower == "manager" || lower == "auto" || lower == "orchestrator" || lower == "ti_manager" {
        *mode = ChatMode::Orchestrator;
        tui_hud::set_mode_orchestrator(default_mgr_model, default_mgr_max_iters);
        tui_hud::set_kv_budget(
            config.context_budget.max_context_tokens,
            config.context_budget.soft_limit_tokens,
        );
        tui_hud::push_notice(
            0,
            false,
            &format!(
                "👔 Modo restaurado a Orquestador (`ti_manager` → `{}`). Tu historial del Manager sigue intacto.",
                default_mgr_model
            ),
        );
        return;
    }

    let live_models = fetch_live_models(client).await;
    let running = client.running_models().await.unwrap_or_default();

    let mut body = String::new();
    match mode {
        ChatMode::Orchestrator => {
            body.push_str(&format!(
                "Modo activo: Orquestador (`ti_manager`) → `{}`\n\n",
                default_mgr_model
            ));
        }
        ChatMode::DirectModel(m) => {
            body.push_str(&format!(
                "Modo activo: Directo (Sin Orquestador) → `{}`\n\n",
                m
            ));
        }
    }

    if let Some(ref models) = live_models {
        body.push_str("Modelos disponibles en llama-swap (`GET /v1/models`):\n");
        for m in models {
            let swap_state = running
                .iter()
                .find(|(rm, _)| rm == m)
                .map(|(_, st)| format!(" [en VRAM/RAM: {}]", st))
                .unwrap_or_default();
            let marker = match mode {
                ChatMode::DirectModel(active) if active == m => " ◀ activo (directo)",
                ChatMode::Orchestrator if m == default_mgr_model => " ◀ orquestador",
                _ => "",
            };
            body.push_str(&format!("  • {}{}{}\n", m, swap_state, marker));
        }
    } else {
        let mut cfg_models = BTreeSet::new();
        for a in config.agents.values() {
            cfg_models.insert(a.model.clone());
        }
        body.push_str(&format!(
            "⚠️ llama-swap no responde en {}. Modelos vistos en config.yaml:\n",
            config.server.base_url
        ));
        for m in cfg_models {
            body.push_str(&format!("  • {}\n", m));
        }
    }

    body.push_str("\nUso:\n  • `/model <id>`    → Precarga y activa chat directo con ese modelo (hot-swap en llama-swap)\n  • `/model manager` → Regresar al `ti_manager` preservando su sesión");
    tui_hud::push_card("🧠", "SELECTOR DE MODELO Y MODO", "llama-swap", &body);
}

const DIRECT_MODEL_MAX_STEPS: usize = 24;

const DIRECT_MODEL_TOOLS: &[&str] = &[
    "list_files",
    "find_files",
    "read_file",
    "write_file",
    "apply_diff",
    "search_code",
    "shell_exec",
    "web_rag",
    "vision_inspect",
    "list_skills",
    "load_skill",
    "project_memory",
    "clarify",
    "git_diff",
    "git_checkpoint",
    "bg_start",
    "bg_logs",
    "bg_kill",
    "update_plan",
];

fn build_direct_model_system_prompt(model_id: &str, workspace: &Path) -> String {
    let skills_catalog = format_skills_catalog(workspace);
    format!(
        "Eres un agente técnico autónomo ejecutándote en Modo Directo sobre un único modelo (`{}`) sin orquestador multi-agente ni subagentes.\n\
         Workspace actual: {}\n\
         {}\n\
         {}\n\
         REGLAS DE EJECUCIÓN EN MODO DIRECTO:\n\
         1. Tienes acceso real a herramientas locales (`web_rag`, `shell_exec`, `read_file`, `write_file`, `apply_diff`, `find_files`, `search_code`, `list_files`, `list_skills`, `load_skill`, `project_memory`, `clarify`, `vision_inspect`, etc.).\n\
         2. NUNCA imprimas bloques de código PowerShell/bash esperando que el usuario los ejecute manualmente si puedes ejecutar la herramienta tú mismo con `shell_exec`, `web_rag` o herramientas de archivos.\n\
         3. Si una skill fue activada o decides usar `load_skill`, sigue TODOS sus pasos ejecutando tus herramientas de forma iterativa hasta concluir completamente el objetivo y entregar el resultado final verificado.",
        model_id,
        workspace.display(),
        runtime_os_shell_hint(),
        skills_catalog
    )
}

async fn run_direct_single_model_turn(
    client: &LlamaClient,
    config: &HarnessConfig,
    workspace: &Path,
    model_id: &str,
    input: &str,
    direct_histories: &mut HashMap<String, Vec<ChatMessage>>,
) {
    let history = direct_histories.entry(model_id.to_string()).or_insert_with(|| {
        vec![ChatMessage::system(build_direct_model_system_prompt(
            model_id, workspace,
        ))]
    });

    let user_msg = if let Some((matched, body)) = match_triggered_skills(workspace, input, 2) {
        tui_hud::push_notice(0, false, &format!("📚 skills (directo) · {}", matched.join(", ")));
        format!(
            "{}\n\n{}\n\nInstrucción del usuario:\n{}",
            body,
            "Sigue esta skill paso a paso usando tus tools reales hasta concluir.",
            input
        )
    } else {
        input.to_string()
    };

    let prev_len = history.len();
    history.push(ChatMessage::user(user_msg));

    let allowed: Vec<String> = DIRECT_MODEL_TOOLS.iter().map(|s| (*s).to_string()).collect();
    let tools_schema = get_tools_schema(&allowed);
    let mut loop_guard = LoopGuard::new(2);
    let t_turn = Instant::now();

    for step in 1..=DIRECT_MODEL_MAX_STEPS {
        tui_hud::set_activity(format!(
            "⚡ {} (Paso {}/{})",
            model_id, step, DIRECT_MODEL_MAX_STEPS
        ));

        let resp = match client
            .chat_completion(
                model_id,
                history,
                Some(tools_schema.clone()),
                0.3,
                4096,
                config.context_budget.strip_historical_thinking,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                if step == 1 {
                    history.truncate(prev_len);
                }
                tui_hud::set_activity(format!("⚠️ Error en `{}`", model_id));
                tui_hud::push_notice(0, true, &format!("❌ Error invocando `{}`: {}", model_id, e));
                return;
            }
        };

        let used = resp.prompt_tokens + resp.completion_tokens;
        tui_hud::set_kv_and_speed(used, resp.tokens_per_sec);

        if !resp.reasoning.trim().is_empty() {
            tui_hud::push_thinking(0, model_id, &resp.reasoning);
        }

        let tool_calls = extract_tolerant_tool_calls(&resp.content, &resp.tool_calls);

        if tool_calls.is_empty() {
            history.push(ChatMessage::assistant(&resp.content));
            tui_hud::set_activity(format!("Chat directo listo ({})", model_id));
            tui_hud::push_card(
                "⚡",
                &format!("RESPUESTA DIRECTA • {}", model_id),
                &format!(
                    "{} pasos · {:.1}s · {:.1} t/s",
                    step,
                    t_turn.elapsed().as_secs_f64(),
                    resp.tokens_per_sec
                ),
                &resp.content,
            );

            maybe_compact_direct_history(
                history,
                resp.prompt_tokens,
                config.context_budget.soft_limit_tokens,
            );
            return;
        }

        history.push(ChatMessage {
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

        for tc in tool_calls {
            let (verb, target) = tui_hud::format_tool_target(&tc.function.name, &tc.function.arguments);
            tui_hud::set_activity(format!(
                "⚡ {} ({}/{}) • {} {}",
                model_id, step, DIRECT_MODEL_MAX_STEPS, verb, target
            ));
            tui_hud::push_action(
                0,
                verb,
                &target,
                &format!("paso {}/{}", step, DIRECT_MODEL_MAX_STEPS),
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
                    tui_hud::push_notice(0, true, &format!("⚠️ {}", guard_msg));
                    guard_msg
                }
            };

            history.push(ChatMessage::tool_result(
                &tc.id,
                &tc.function.name,
                tool_output,
            ));
        }

        maybe_compact_direct_history(
            history,
            resp.prompt_tokens,
            config.context_budget.soft_limit_tokens,
        );

        if step == DIRECT_MODEL_MAX_STEPS {
            tui_hud::set_activity(format!("⚠️ Límite de pasos en `{}`", model_id));
            tui_hud::push_card(
                "⚠️",
                &format!("LÍMITE DIRECTO • {}", model_id),
                &format!("{} pasos · {:.1}s", step, t_turn.elapsed().as_secs_f64()),
                &resp.content,
            );
        }
    }
}

fn maybe_compact_direct_history(
    history: &mut Vec<ChatMessage>,
    prompt_tokens: usize,
    default_soft: usize,
) {
    let soft_limit = tui_hud::current_kv_soft_limit(default_soft);
    if prompt_tokens >= soft_limit && history.len() >= 6 {
        let before = history.len();
        compact_ephemeral_history(history);
        tui_hud::push_notice(
            0,
            false,
            &format!(
                "· Historial directo compactado ({} → {} mensajes · umbral soft {} tk) para mantener velocidad.",
                before,
                history.len(),
                soft_limit
            ),
        );
    }
}

fn restore_hud_idle(mode: &ChatMode) {
    match mode {
        ChatMode::Orchestrator => tui_hud::set_activity("Esperando instrucción"),
        ChatMode::DirectModel(m) => tui_hud::set_activity(format!("Chat directo listo ({})", m)),
    }
}

fn print_welcome_card(config: &HarnessConfig, workspace: &Path) {
    let cfg_src = config
        .loaded_from
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "memoria".to_string());
    let body = format!(
        "• Workspace:   {}\n\
         • Servidor:    {} (config: {})\n\
         • Presupuesto: {} tk max │ Soft limit: {} tk │ Strip <think>: {}\n\
         • Modos:       Instrucción libre → `ti_manager` │ `@agente <tarea>` → Subagente efímero\n\
         • Atajos UI:   `Ctrl+O` (o `/think`) expandir `<think>` │ Rueda / `PgUp` / `PgDn` scroll │ `Ctrl+C` cancelar/salir\n\
         • Comandos:    `/help`, `/model`, `/plan`, `/resume`, `/agents`, `/skills`, `/vision`, `/rag`, `/status`, `/clear`, `/exit`",
        workspace.display(),
        config.server.base_url,
        cfg_src,
        config.context_budget.max_context_tokens,
        config.context_budget.soft_limit_tokens,
        config.context_budget.strip_historical_thinking
    );
    tui_hud::push_card("⚡", "OPENED-AI INTERACTIVE TUI", "Alternate Screen Activo", &body);
}

fn print_help() {
    let body = "\
MODOS DE EJECUCIÓN
  • <mensaje libre>             → Ejecuta el Orquestador `ti_manager` (o modelo directo si usaste `/model <id>`)
  • @<agente> <tarea>           → Ejecuta un subagente efímero directo (con sus tools, sin `ti_manager`)
  • /model                      → Lista los modelos reales registrados en `llama-swap` (`:8080`)
  • /model <id>                 → Cambia a chat directo con un solo modelo (sin orquestador, hot-swap automático)
  • /model manager              → Regresa al Orquestador `ti_manager` manteniendo su sesión

NAVEGACIÓN, RAZONAMIENTO Y SESIÓN
  • Ctrl+O  (o `/think`)        → Expande / colapsa todas las cadenas `<think>` en pantalla
  • Rueda / PgUp / PgDn / ▲ ▼   → Scroll en el historial sin mover el encabezado fijo
  • Alt+▲ / Alt+▼               → Historial de prompts enviados anteriormente
  • Ctrl+C                      → Si está ejecutando: cancela la corrida actual; si está libre: sale a terminal
  • /plan                       → Muestra la barra de avance y `.harness/PLAN.md`
  • /resume                     → Reanuda la última sesión guardada en `.harness/manager_session.json`
  • /clear                      → Limpia memoria del modo activo (y sesión guardada en modo Orquestador)
  • /exit  (o /quit)            → Sale del TUI restaurando la terminal intacta

INSPECCIÓN Y HERRAMIENTAS
  • /status                     → Comprueba conexión y modelos en `llama-swap`
  • /agents                     → Lista los agentes definidos en `config/config.yaml`
  • /skills                     → Lista las skills de carga perezosa (`skills/*/SKILL.md`)
  • /vision <url|html> [prompt] → Captura Headless Chrome + auditoría DOM + Visión multimodal
  • /rag <consulta|url>         → Búsqueda técnica multi-fuente (anti-CAPTCHA) o URL a Markdown";
    tui_hud::push_card("📖", "GUÍA DE COMANDOS OPENED-AI", "v0.1", body);
}
