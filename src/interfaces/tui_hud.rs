use crossterm::terminal;
use serde_json::Value;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static IS_STREAMING: AtomicBool = AtomicBool::new(false);

pub const HUD_ROWS: u16 = 6;
pub const FOOTER_ROWS: u16 = 3;
const MAX_INPUT_LINES: usize = 3;
const MAX_PICKER_VISIBLE: usize = 6;
const MIN_HUD_COLS: u16 = 56;
const MIN_HUD_ROWS: u16 = 14;
const MAX_TRANSCRIPT_ITEMS: usize = 600;

#[derive(Debug, Clone)]
pub enum TranscriptItem {
    UserPrompt(String),
    GroupHeader {
        icon: String,
        title: String,
        meta: String,
    },
    Action {
        indent: usize,
        verb: String,
        target: String,
        meta: String,
    },
    Thinking {
        indent: usize,
        actor: String,
        turns: usize,
        first_tag: String,
        last_tag: String,
        chars: usize,
        text: String,
    },
    GroupFooter {
        indent: usize,
        icon: String,
        summary: String,
    },
    Card {
        icon: String,
        title: String,
        meta: String,
        body: String,
    },
    Notice {
        indent: usize,
        is_warn: bool,
        text: String,
    },
}

#[derive(Debug, Clone)]
pub struct HudState {
    pub workspace_name: String,
    pub workspace_path: PathBuf,
    pub endpoint: String,
    pub server_online: Option<bool>,
    pub available_models_count: usize,
    pub mode_label: String,
    pub active_model: String,
    pub is_direct_mode: bool,
    pub last_tps: f64,
    pub kv_used: usize,
    pub kv_max: usize,
    pub kv_soft_limit: usize,
    pub kv_saved: usize,
    pub subagents_count: usize,
    pub plan_done: usize,
    pub plan_total: usize,
    pub current_phase: String,
    pub activity: String,
    pub show_thinking: bool,
    pub scroll_from_bottom: usize,
    pub last_max_scroll: usize,
    pub input_text: String,
    pub input_cursor_chars: usize,
    pub prompt_prefix: String,
    pub is_busy: bool,
    pub awaiting_clarify: bool,
    pub alt_screen_active: bool,
    pub model_picker_open: bool,
    pub model_picker_models: Vec<String>,
    pub model_picker_running: Vec<(String, String)>,
    pub model_picker_selected: usize,
    pub entries: Vec<TranscriptItem>,
}

impl Default for HudState {
    fn default() -> Self {
        Self {
            workspace_name: "workspace".to_string(),
            workspace_path: PathBuf::from("."),
            endpoint: "http://127.0.0.1:8080/v1".to_string(),
            server_online: None,
            available_models_count: 0,
            mode_label: "Orquestador (TI Manager • ∞)".to_string(),
            active_model: "qwen38-distill".to_string(),
            is_direct_mode: false,
            last_tps: 0.0,
            kv_used: 0,
            kv_max: 32768,
            kv_soft_limit: 20480,
            kv_saved: 0,
            subagents_count: 0,
            plan_done: 0,
            plan_total: 0,
            current_phase: "Sin .harness/PLAN.md".to_string(),
            activity: "Esperando instrucción".to_string(),
            show_thinking: false,
            scroll_from_bottom: 0,
            last_max_scroll: 0,
            input_text: String::new(),
            input_cursor_chars: 0,
            prompt_prefix: "⚡ oai".to_string(),
            is_busy: false,
            awaiting_clarify: false,
            alt_screen_active: false,
            model_picker_open: false,
            model_picker_models: Vec::new(),
            model_picker_running: Vec::new(),
            model_picker_selected: 0,
            entries: Vec::new(),
        }
    }
}

static HUD: OnceLock<Mutex<Option<HudState>>> = OnceLock::new();
static INTERACTIVE_CLARIFY_ALLOWED: AtomicBool = AtomicBool::new(false);

struct PendingClarify {
    options: Vec<String>,
    reply_tx: tokio::sync::oneshot::Sender<String>,
}

static PENDING_CLARIFY: OnceLock<Mutex<Option<PendingClarify>>> = OnceLock::new();

fn clarify_lock() -> &'static Mutex<Option<PendingClarify>> {
    PENDING_CLARIFY.get_or_init(|| Mutex::new(None))
}

fn hud_lock() -> &'static Mutex<Option<HudState>> {
    HUD.get_or_init(|| Mutex::new(None))
}

/// Guard RAII para Alternate Screen (`\x1b[?1049h`), Alternate Scroll (`\x1b[?1007h`)
/// y raw mode. Restaura PowerShell/terminal intacta al salir o en `Ctrl+C`.
pub struct HudGuard;

impl HudGuard {
    pub fn enter(
        workspace: &Path,
        endpoint: &str,
        default_model: &str,
        kv_max: usize,
        kv_soft_limit: usize,
        max_iterations: usize,
    ) -> Self {
        enable_vt_support();

        let is_tty = io::stdout().is_terminal() && io::stderr().is_terminal();
        if !is_tty {
            return Self;
        }

        let ws_name = workspace
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| workspace.display().to_string());

        let mode_label = if max_iterations == 0 {
            "Orquestador (TI Manager • ∞)".to_string()
        } else {
            format!("Orquestador (TI Manager • {}t)", max_iterations)
        };

        let mut state = HudState {
            workspace_name: ws_name,
            workspace_path: workspace.to_path_buf(),
            endpoint: endpoint.to_string(),
            mode_label,
            active_model: default_model.to_string(),
            kv_max: kv_max.max(1),
            kv_soft_limit,
            ..Default::default()
        };

        refresh_plan_fields(&mut state);

        if terminal::enable_raw_mode().is_ok() {
            let mut out = io::stdout().lock();
            // Alternate screen buffer + Alternate scroll mode (rueda envía Up/Down sin bloquear selección mouse)
            let _ = write!(out, "\x1b[?1049h\x1b[?1007h\x1b[2J\x1b[H");
            let _ = out.flush();
            state.alt_screen_active = true;
        }

        if let Ok(mut guard) = hud_lock().lock() {
            render_locked(&mut state);
            *guard = Some(state);
        }

        Self
    }
}

impl Drop for HudGuard {
    fn drop(&mut self) {
        cleanup_screen();
    }
}

pub fn cleanup_screen() {
    cancel_clarification();
    INTERACTIVE_CLARIFY_ALLOWED.store(false, Ordering::SeqCst);
    if let Ok(mut guard) = hud_lock().lock() {
        if let Some(ref mut state) = *guard {
            if state.alt_screen_active {
                let mut out = io::stdout().lock();
                let _ = write!(out, "\x1b[?7h\x1b[?25h\x1b[?1007l\x1b[?1049l\x1b[0m");
                let _ = out.flush();
                let _ = terminal::disable_raw_mode();
                state.alt_screen_active = false;
            }
        }
        *guard = None;
    }
}

pub fn is_active() -> bool {
    hud_lock()
        .lock()
        .map(|g| g.as_ref().map(|s| s.alt_screen_active).unwrap_or(false))
        .unwrap_or(false)
}

pub fn with_update<F: FnOnce(&mut HudState)>(updater: F) {
    if let Ok(mut guard) = hud_lock().lock() {
        if let Some(ref mut state) = *guard {
            updater(state);
            render_locked(state);
        }
    }
}

pub fn redraw() {
    with_update(|_| {});
}

pub fn sync_plan(workspace: &Path) {
    with_update(|s| {
        s.workspace_path = workspace.to_path_buf();
        refresh_plan_fields(s);
    });
}

pub fn set_server_status(online: bool, models_count: usize) {
    with_update(|s| {
        s.server_online = Some(online);
        s.available_models_count = models_count;
    });
}

fn default_prompt_prefix(s: &HudState) -> String {
    if s.is_direct_mode {
        format!("⚡ {}", truncate_chars(&s.active_model, 16))
    } else {
        "⚡ oai".to_string()
    }
}

pub fn set_mode_orchestrator(model: &str, max_iterations: usize) {
    with_update(|s| {
        s.is_direct_mode = false;
        s.prompt_prefix = "⚡ oai".to_string();
        s.mode_label = if max_iterations == 0 {
            "Orquestador (TI Manager • ∞)".to_string()
        } else {
            format!("Orquestador (TI Manager • {}t)", max_iterations)
        };
        s.active_model = model.to_string();
        s.activity = "Esperando instrucción".to_string();
    });
}

pub fn set_mode_direct(model: &str) {
    with_update(|s| {
        s.is_direct_mode = true;
        s.prompt_prefix = format!("⚡ {}", truncate_chars(model, 16));
        s.mode_label = "Directo (Solo Modelo • Sin Orquestador)".to_string();
        s.active_model = model.to_string();
        s.activity = "Chat directo listo".to_string();
    });
}

pub fn set_kv_and_speed(kv_used: usize, tps: f64) {
    with_update(|s| {
        s.kv_used = kv_used;
        if tps > 0.0 {
            s.last_tps = tps;
        }
    });
}

/// Actualiza el techo máximo de KV cache y el umbral soft mostrados en el HUD.
pub fn set_kv_budget(kv_max: usize, kv_soft_limit: usize) {
    with_update(|s| {
        s.kv_max = kv_max.max(1);
        s.kv_soft_limit = kv_soft_limit;
    });
}

/// Devuelve el umbral soft activo en el HUD (fallback si aún no está inicializado).
pub fn current_kv_soft_limit(fallback: usize) -> usize {
    hud_lock()
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.kv_soft_limit))
        .filter(|&limit| limit > 0)
        .unwrap_or(fallback)
}

pub fn set_speed(tps: f64) {
    if tps > 0.0 {
        with_update(|s| {
            s.last_tps = tps;
        });
    }
}

pub fn set_streaming_progress(tokens_generated: usize, live_tps: f64) {
    IS_STREAMING.store(true, Ordering::SeqCst);
    with_update(|s| {
        if live_tps > 0.0 {
            s.last_tps = live_tps;
        }
        s.activity = format!("✍️ Generando… {} tk ({:.1} t/s)", tokens_generated, live_tps);
    });
}

/// Actualiza el indicador de espera mientras aún no llegan tokens por SSE.
pub fn is_streaming() -> bool {
    IS_STREAMING.load(Ordering::SeqCst)
}

pub fn set_waiting_activity(activity: impl Into<String>) {
    if IS_STREAMING.load(Ordering::SeqCst) {
        return;
    }
    let act = activity.into();
    with_update(|s| {
        s.activity = act;
    });
}

pub fn add_saved_kv(saved_delta: usize, subagents_delta: usize) {
    with_update(|s| {
        s.kv_saved += saved_delta;
        s.subagents_count += subagents_delta;
    });
}

pub fn set_activity(activity: impl Into<String>) {
    IS_STREAMING.store(false, Ordering::SeqCst);
    let act = activity.into();
    with_update(|s| {
        s.activity = act;
    });
}

pub fn set_input(text: &str, cursor_chars: usize, is_busy: bool) {
    with_update(|s| {
        s.input_text = text.to_string();
        s.input_cursor_chars = cursor_chars;
        s.is_busy = is_busy;
        let trimmed = text.trim_start();
        let should_open = !is_busy && (trimmed == "/model" || trimmed.starts_with("/model "));
        s.model_picker_open = should_open;
        if should_open {
            let count = filtered_model_picker_candidates(s).len();
            if count == 0 || s.model_picker_selected >= count {
                s.model_picker_selected = 0;
            }
        } else {
            s.model_picker_selected = 0;
        }
    });
}

pub fn set_model_picker_catalog(models: Vec<String>, running: Vec<(String, String)>) {
    with_update(|s| {
        s.model_picker_models = models;
        s.model_picker_running = running;
        let count = filtered_model_picker_candidates(s).len();
        if count == 0 || s.model_picker_selected >= count {
            s.model_picker_selected = 0;
        }
    });
}

pub fn is_model_picker_open() -> bool {
    hud_lock()
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.model_picker_open))
        .unwrap_or(false)
}

pub fn move_model_picker_selection(delta: isize) {
    with_update(|s| {
        if !s.model_picker_open {
            return;
        }
        let count = filtered_model_picker_candidates(s).len();
        if count == 0 {
            s.model_picker_selected = 0;
            return;
        }
        let cur = s.model_picker_selected.min(count - 1) as isize;
        let next = (cur + delta).rem_euclid(count as isize) as usize;
        s.model_picker_selected = next;
    });
}

/// Autocompleta con `Tab` (o cicla si la coincidencia actual ya está escrita completa).
pub fn autocomplete_model_picker(reverse: bool) -> Option<String> {
    let mut guard = hud_lock().lock().ok()?;
    let state = guard.as_mut()?;
    if !state.model_picker_open {
        return None;
    }
    let candidates = filtered_model_picker_candidates(state);
    if candidates.is_empty() {
        return None;
    }

    let filter = model_picker_filter_query(&state.input_text);
    let mut idx = state.model_picker_selected.min(candidates.len() - 1);
    if !filter.is_empty() && candidates[idx].to_lowercase() == filter {
        let delta = if reverse { -1isize } else { 1isize };
        idx = (idx as isize + delta).rem_euclid(candidates.len() as isize) as usize;
        state.model_picker_selected = idx;
    }

    let completed = format!("/model {}", candidates[idx]);
    state.input_text = completed.clone();
    state.input_cursor_chars = completed.chars().count();
    render_locked(state);
    Some(completed)
}

/// Devuelve el candidato actualmente resaltado en el selector `/model`.
pub fn selected_model_picker_value() -> Option<String> {
    let guard = hud_lock().lock().ok()?;
    let state = guard.as_ref()?;
    if !state.model_picker_open {
        return None;
    }
    let candidates = filtered_model_picker_candidates(state);
    if candidates.is_empty() {
        None
    } else {
        Some(candidates[state.model_picker_selected.min(candidates.len() - 1)].clone())
    }
}

fn model_picker_filter_query(input_text: &str) -> String {
    input_text
        .trim_start()
        .strip_prefix("/model")
        .unwrap_or("")
        .trim()
        .to_lowercase()
}

fn filtered_model_picker_candidates(state: &HudState) -> Vec<String> {
    let mut all = state.model_picker_models.clone();
    if !all.iter().any(|m| m == &state.active_model) && !state.active_model.is_empty() {
        all.push(state.active_model.clone());
    }
    if !all.iter().any(|m| m == "manager") {
        all.push("manager".to_string());
    }

    let query = model_picker_filter_query(&state.input_text);
    if query.is_empty() {
        return all;
    }

    let mut starts_with = Vec::new();
    let mut contains = Vec::new();
    for item in all {
        let low = item.to_lowercase();
        if low.starts_with(&query) {
            starts_with.push(item);
        } else if low.contains(&query) {
            contains.push(item);
        }
    }
    starts_with.extend(contains);
    starts_with
}

pub fn set_interactive_clarify_enabled(enabled: bool) {
    INTERACTIVE_CLARIFY_ALLOWED.store(enabled, Ordering::SeqCst);
}

pub fn is_awaiting_clarify() -> bool {
    clarify_lock()
        .lock()
        .map(|g| g.is_some())
        .unwrap_or(false)
}

pub fn submit_clarification(raw_answer: &str) {
    let trimmed = raw_answer.trim();
    if trimmed.is_empty() {
        return;
    }
    let pending = clarify_lock().lock().ok().and_then(|mut g| g.take());
    if let Some(p) = pending {
        let resolved = if let Ok(idx) = trimmed.parse::<usize>() {
            if idx >= 1 && idx <= p.options.len() {
                format!("{}. {}", idx, p.options[idx - 1])
            } else {
                trimmed.to_string()
            }
        } else {
            trimmed.to_string()
        };
        with_update(|s| {
            s.awaiting_clarify = false;
            s.prompt_prefix = default_prompt_prefix(s);
            s.input_text.clear();
            s.input_cursor_chars = 0;
        });
        push_notice(0, false, &format!("💬 Respuesta usuario: {}", resolved));
        let _ = p.reply_tx.send(resolved);
    }
}

pub fn cancel_clarification() {
    if let Ok(mut guard) = clarify_lock().lock() {
        *guard = None;
    }
    with_update(|s| {
        s.awaiting_clarify = false;
        s.prompt_prefix = default_prompt_prefix(s);
    });
}

pub async fn request_clarification(question: &str, options: &[String]) -> String {
    if !INTERACTIVE_CLARIFY_ALLOWED.load(Ordering::SeqCst) {
        return "[MODO NO INTERACTIVO]: No hay consola interactiva disponible para preguntar al usuario. Elige la opción técnica más razonable y segura, documéntala en tu respuesta y continúa.".to_string();
    }

    let mut card_body = question.trim().to_string();
    if !options.is_empty() {
        card_body.push_str("\n\nOpciones sugeridas (escribe el número o tu respuesta):");
        for (i, opt) in options.iter().enumerate() {
            card_body.push_str(&format!("\n  {}. {}", i + 1, opt));
        }
    }

    if is_active() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if let Ok(mut guard) = clarify_lock().lock() {
            *guard = Some(PendingClarify {
                options: options.to_vec(),
                reply_tx: tx,
            });
        }
        push_card("❓", "ACLARACIÓN SOLICITADA", "Esperando respuesta", &card_body);
        with_update(|s| {
            s.awaiting_clarify = true;
            s.prompt_prefix = "❓ Responde".to_string();
            s.input_text.clear();
            s.input_cursor_chars = 0;
            s.activity = "❓ Esperando aclaración del usuario".to_string();
        });
        match rx.await {
            Ok(ans) => format!("Respuesta del usuario: {}", ans),
            Err(_) => "[Aclaración cancelada por el usuario]".to_string(),
        }
    } else if io::stdin().is_terminal() {
        println!("\n❓ ACLARACIÓN SOLICITADA:\n{}\n", card_body);
        let opts_owned = options.to_vec();
        tokio::task::spawn_blocking(move || {
            print!("❓ Tu respuesta> ");
            let _ = io::stdout().flush();
            let mut line = String::new();
            if io::stdin().read_line(&mut line).is_ok() {
                let t = line.trim();
                if let Ok(idx) = t.parse::<usize>() {
                    if idx >= 1 && idx <= opts_owned.len() {
                        return format!("Respuesta del usuario: {}. {}", idx, opts_owned[idx - 1]);
                    }
                }
                if !t.is_empty() {
                    return format!("Respuesta del usuario: {}", t);
                }
            }
            "[Sin respuesta del usuario; procede con el criterio más seguro]".to_string()
        })
        .await
        .unwrap_or_else(|_| "[Sin respuesta del usuario]".to_string())
    } else {
        "[MODO NO INTERACTIVO]: stdin no es TTY. Elige la opción técnica más segura y continúa.".to_string()
    }
}

pub fn toggle_thinking() -> bool {
    let mut now = false;
    with_update(|s| {
        s.show_thinking = !s.show_thinking;
        now = s.show_thinking;
    });
    now
}

pub fn scroll_up(lines: usize) {
    with_update(|s| {
        s.scroll_from_bottom = s.scroll_from_bottom.saturating_add(lines).min(s.last_max_scroll);
    });
}

pub fn scroll_down(lines: usize) {
    with_update(|s| {
        s.scroll_from_bottom = s.scroll_from_bottom.saturating_sub(lines);
    });
}

pub fn scroll_to_bottom() {
    with_update(|s| {
        s.scroll_from_bottom = 0;
    });
}

pub fn clear_transcript() {
    with_update(|s| {
        s.entries.clear();
        s.scroll_from_bottom = 0;
    });
}

fn push_item(item: TranscriptItem) {
    if let Ok(mut guard) = hud_lock().lock() {
        if let Some(ref mut state) = *guard {
            if state.entries.len() >= MAX_TRANSCRIPT_ITEMS {
                let drop_n = state.entries.len() - MAX_TRANSCRIPT_ITEMS + 1;
                state.entries.drain(..drop_n);
            }
            state.entries.push(item);
            render_locked(state);
        }
    }
}

pub fn push_user_prompt(text: &str) {
    if is_active() {
        scroll_to_bottom();
        push_item(TranscriptItem::UserPrompt(text.to_string()));
    }
}

pub fn push_group_header(icon: &str, title: &str, meta: &str) {
    if is_active() {
        push_item(TranscriptItem::GroupHeader {
            icon: icon.to_string(),
            title: title.to_string(),
            meta: meta.to_string(),
        });
    } else if meta.is_empty() {
        eprintln!("\n{} {}", icon, title);
    } else {
        eprintln!("\n{} {} ({})", icon, title, meta);
    }
}

pub fn push_action(indent: usize, verb: &str, target: &str, meta: &str) {
    if is_active() {
        push_item(TranscriptItem::Action {
            indent,
            verb: verb.to_string(),
            target: target.to_string(),
            meta: meta.to_string(),
        });
    } else {
        let pad = "  ".repeat(indent + 1);
        if meta.is_empty() {
            eprintln!("{}├─ {:<8} {}", pad, verb, target);
        } else {
            eprintln!("{}├─ {:<8} {} ({})", pad, verb, target, meta);
        }
    }
}

pub fn push_thinking(indent: usize, actor: &str, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let chars = trimmed.chars().count();
    let (actor_base, turn_tag) = split_actor_turn(actor);
    if is_active() {
        if let Ok(mut guard) = hud_lock().lock() {
            if let Some(ref mut state) = *guard {
                if coalesce_thinking_locked(
                    &mut state.entries,
                    indent,
                    &actor_base,
                    turn_tag.as_deref(),
                    chars,
                    trimmed,
                ) {
                    render_locked(state);
                    return;
                }
            }
        }
        let formatted_text = match turn_tag.as_deref() {
            Some(tag) => format!("[{}] {}", tag, trimmed),
            None => trimmed.to_string(),
        };
        push_item(TranscriptItem::Thinking {
            indent,
            actor: actor_base,
            turns: 1,
            first_tag: turn_tag.clone().unwrap_or_default(),
            last_tag: turn_tag.unwrap_or_default(),
            chars,
            text: formatted_text,
        });
    } else {
        let preview: String = trimmed.chars().take(180).collect();
        let pad = "  ".repeat(indent + 1);
        let label = match turn_tag {
            Some(tag) => format!("{} {}", actor_base, tag),
            None => actor_base,
        };
        eprintln!("{}🧠 {} ({} chars): {}", pad, label, chars, preview.replace('\n', " "));
    }
}

fn split_actor_turn(raw: &str) -> (String, Option<String>) {
    let trimmed = raw.trim();
    if let Some((base, suffix)) = trimmed.rsplit_once(' ') {
        let s = suffix.trim();
        if let Some(rest) = s.strip_prefix('t') {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                return (base.to_string(), Some(s.to_string()));
            }
        }
    }
    (trimmed.to_string(), None)
}

fn coalesce_thinking_locked(
    entries: &mut [TranscriptItem],
    indent: usize,
    actor_base: &str,
    turn_tag: Option<&str>,
    chars: usize,
    text: &str,
) -> bool {
    for item in entries.iter_mut().rev() {
        match item {
            TranscriptItem::UserPrompt(_) | TranscriptItem::GroupHeader { .. } | TranscriptItem::Card { .. } => {
                return false;
            }
            TranscriptItem::GroupFooter { indent: f_indent, .. } if *f_indent <= indent => {
                return false;
            }
            TranscriptItem::GroupFooter { .. } => {
                // Footer de un subagente hijo (`f_indent > indent`) rompe la continuidad del padre
                return false;
            }
            TranscriptItem::Action { indent: a_indent, .. }
            | TranscriptItem::Notice { indent: a_indent, .. } => {
                if *a_indent != indent {
                    return false;
                }
            }
            TranscriptItem::Thinking {
                indent: t_indent,
                actor,
                turns,
                first_tag: _,
                last_tag,
                chars: existing_chars,
                text: existing_text,
            } => {
                if *t_indent != indent || actor != actor_base {
                    return false;
                }
                *turns += 1;
                *existing_chars += chars;
                let step_label = turn_tag
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| format!("paso {}", *turns));
                if let Some(t) = turn_tag {
                    *last_tag = t.to_string();
                } else {
                    *last_tag = step_label.clone();
                }
                if !existing_text.is_empty() {
                    existing_text.push('\n');
                }
                existing_text.push_str(&format!("[{}] {}", step_label, text));
                return true;
            }
        }
    }
    false
}

pub fn push_group_footer(indent: usize, icon: &str, summary: &str) {
    if is_active() {
        push_item(TranscriptItem::GroupFooter {
            indent,
            icon: icon.to_string(),
            summary: summary.to_string(),
        });
    } else {
        let pad = "  ".repeat(indent);
        eprintln!("{}└─ {} {}", pad, icon, summary);
    }
}

pub fn push_card(icon: &str, title: &str, meta: &str, body: &str) {
    if is_active() {
        push_item(TranscriptItem::Card {
            icon: icon.to_string(),
            title: title.to_string(),
            meta: meta.to_string(),
            body: body.to_string(),
        });
    } else {
        if meta.is_empty() {
            println!("\n╭─ {} {} ─────────────────────", icon, title);
        } else {
            println!("\n╭─ {} {} ─ {} ─────────────", icon, title, meta);
        }
        println!("{}", body);
        println!("╰──────────────────────────────────────────────────\n");
    }
}

pub fn push_notice(indent: usize, is_warn: bool, text: &str) {
    if is_active() {
        push_item(TranscriptItem::Notice {
            indent,
            is_warn,
            text: text.to_string(),
        });
    } else {
        let pad = "  ".repeat(indent);
        eprintln!("{}{}", pad, text);
    }
}

/// Convierte una llamada a herramienta + JSON de argumentos en un par compacto `(Verbo, Objetivo)`
/// estilo lista consolidada (Read / Edit / Write / Run / Search / Delegate).
pub fn format_tool_target(tool_name: &str, raw_args: &str) -> (&'static str, String) {
    let args: Value = serde_json::from_str(raw_args).unwrap_or(Value::Null);
    match tool_name {
        "read_file" => {
            let path = args["path"].as_str().unwrap_or("?");
            let start = args["start_line"].as_u64();
            let end = args["end_line"].as_u64();
            let loc = match (start, end) {
                (Some(s), Some(e)) => format!("{} #L{}-{}", path, s, e),
                (Some(s), None) => format!("{} #L{}+", path, s),
                _ => path.to_string(),
            };
            ("Read", loc)
        }
        "apply_diff" => ("Edit", args["path"].as_str().unwrap_or("?").to_string()),
        "write_file" => {
            let path = args["path"].as_str().unwrap_or("?");
            let bytes = args["content"].as_str().map(|s| s.len()).unwrap_or(0);
            ("Write", format!("{} ({} B)", path, bytes))
        }
        "shell_exec" => {
            let cmd = args["command"].as_str().unwrap_or("?");
            ("Run", truncate_chars(cmd, 68))
        }
        "search_code" => {
            let q = args["query"].as_str().unwrap_or("?");
            if let Some(glob) = args["glob"].as_str().filter(|s| !s.is_empty()) {
                ("Search", format!("\"{}\" [{}]", truncate_chars(q, 38), truncate_chars(glob, 14)))
            } else {
                ("Search", format!("\"{}\"", truncate_chars(q, 52)))
            }
        }
        "find_files" => {
            let pat = args["pattern"].as_str().filter(|s| !s.is_empty()).unwrap_or("*");
            ("FindFile", truncate_chars(pat, 52))
        }
        "project_memory" => {
            let act = args["action"].as_str().unwrap_or("read");
            ("Memory", format!("{} .harness/MEMORY.md", act))
        }
        "clarify" => {
            let q = args["question"].as_str().unwrap_or("?");
            ("Clarify", truncate_chars(q, 56))
        }
        "list_files" => {
            let sub = args["subdir"].as_str().filter(|s| !s.is_empty()).unwrap_or(".");
            ("List", sub.to_string())
        }
        "delegate_subagent" => {
            let ag = args["agent_name"]
                .as_str()
                .or_else(|| args["subagent"].as_str())
                .unwrap_or("semi_sr_dev");
            ("Delegate", format!("@{}", ag))
        }
        "update_plan" => ("Plan", "Actualizar .harness/PLAN.md".to_string()),
        "git_checkpoint" => (
            "Checkpoint",
            truncate_chars(args["message"].as_str().unwrap_or("snapshot"), 48),
        ),
        "git_diff" => ("GitDiff", "Inspeccionar cambios".to_string()),
        "web_rag" => {
            let target = args["url"]
                .as_str()
                .or_else(|| args["query"].as_str())
                .unwrap_or("?");
            ("WebRAG", truncate_chars(target, 60))
        }
        "vision_inspect" => {
            let t = args["target"].as_str().unwrap_or("?");
            let vp = args["viewport"].as_str().unwrap_or("desktop");
            ("Vision", format!("{} ({})", truncate_chars(t, 44), vp))
        }
        "load_skill" => (
            "Skill",
            args["skill_name"].as_str().unwrap_or("?").to_string(),
        ),
        "list_skills" => ("Skills", "Descubrir skills".to_string()),
        "bg_start" => ("BgStart", args["name"].as_str().unwrap_or("proc").to_string()),
        "bg_logs" => ("BgLogs", args["name"].as_str().unwrap_or("proc").to_string()),
        "bg_kill" => ("BgKill", args["name"].as_str().unwrap_or("proc").to_string()),
        _ => ("Tool", tool_name.to_string()),
    }
}

fn refresh_plan_fields(state: &mut HudState) {
    let plan_path = state.workspace_path.join(".harness").join("PLAN.md");
    let Ok(content) = fs::read_to_string(&plan_path) else {
        state.plan_done = 0;
        state.plan_total = 0;
        state.current_phase = "Sin .harness/PLAN.md (usa TI Manager o /plan)".to_string();
        return;
    };

    // 1. Priorizar checkboxes Markdown estándar `- [x]` / `- [ ]`
    let mut cb_done = 0usize;
    let mut cb_pending = 0usize;
    let mut cb_next: Option<String> = None;
    let mut cb_last: Option<String> = None;

    for line in content.lines() {
        let t = line.trim();
        let lower = t.to_lowercase();
        if lower.starts_with("- [x]") {
            cb_done += 1;
            let clean = t.trim_start_matches("- [x]").trim_start_matches("- [X]").trim();
            if !clean.is_empty() {
                cb_last = Some(clean.to_string());
            }
        } else if lower.starts_with("- [ ]") {
            cb_pending += 1;
            if cb_next.is_none() {
                let clean = t.trim_start_matches("- [ ]").trim();
                if !clean.is_empty() {
                    cb_next = Some(clean.to_string());
                }
            }
        }
    }

    if cb_done + cb_pending > 0 {
        state.plan_done = cb_done;
        state.plan_total = cb_done + cb_pending;
        state.current_phase = if let Some(next) = cb_next {
            format!("Siguiente: {}", next)
        } else if let Some(last) = cb_last {
            format!("Completado ({})", last)
        } else {
            "Todos los hitos completados".to_string()
        };
        return;
    }

    // 2. Fallback limpio para tablas Markdown con `✅` / `🔄` / `⬜` (limpiando pipes `|`)
    let mut tb_done = 0usize;
    let mut tb_pending = 0usize;
    let mut tb_active: Option<String> = None;
    let mut tb_next: Option<String> = None;

    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("|---") || !t.contains('|') {
            continue;
        }
        let has_done = t.contains('✅');
        let has_wip = t.contains("🔄");
        let has_todo = t.contains('⬜');
        if !has_done && !has_wip && !has_todo {
            continue;
        }

        let cols: Vec<String> = t
            .split('|')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(|c| {
                c.replace('✅', "")
                    .replace("🔄", "")
                    .replace('⬜', "")
                    .trim()
                    .to_string()
            })
            .filter(|c| !c.is_empty())
            .collect();

        let label = match cols.as_slice() {
            [num, name, ..] if num.len() <= 4 => format!("Fase {}: {}", num, name),
            [first, ..] => first.clone(),
            [] => String::new(),
        };

        if has_done {
            tb_done += 1;
        } else if has_wip {
            tb_pending += 1;
            if tb_active.is_none() && !label.is_empty() {
                tb_active = Some(format!("En curso: {}", label));
            }
        } else if has_todo {
            tb_pending += 1;
            if tb_next.is_none() && !label.is_empty() {
                tb_next = Some(format!("Siguiente: {}", label));
            }
        }
    }

    state.plan_done = tb_done;
    state.plan_total = tb_done + tb_pending;
    state.current_phase = if state.plan_total == 0 {
        "PLAN.md sin hitos (- [ ] / - [x])".to_string()
    } else if let Some(act) = tb_active {
        act
    } else if let Some(next) = tb_next {
        next
    } else {
        "Todas las fases completadas".to_string()
    };
}

fn render_locked(state: &mut HudState) {
    if !state.alt_screen_active {
        return;
    }
    let (cols, rows) = terminal_size();
    if cols < MIN_HUD_COLS || rows < MIN_HUD_ROWS {
        return;
    }

    let mut out = io::stdout().lock();
    let _ = write!(out, "\x1b[?25l\x1b[?7l");

    // 1. Pintar HUD estático superior (filas 1..=6)
    draw_hud_at_top(&mut out, state, cols);

    // Calcular altura dinámica de input (1..=3 líneas según wrap del texto) y ventana flotante `/model`
    let footer_layout = compute_footer_layout(state, cols);
    let footer_rows = footer_layout.total_rows;
    let picker_candidates = if state.model_picker_open && !state.is_busy {
        filtered_model_picker_candidates(state)
    } else {
        Vec::new()
    };
    let picker_visible_len = if state.model_picker_open && !state.is_busy {
        picker_candidates.len().clamp(1, MAX_PICKER_VISIBLE)
    } else {
        0
    };
    let max_overlay_rows = rows
        .saturating_sub(HUD_ROWS + footer_rows + 3)
        .min((picker_visible_len + 2) as u16);
    let overlay_rows = if state.model_picker_open && !state.is_busy && max_overlay_rows >= 3 {
        max_overlay_rows
    } else {
        0
    };

    // 2. Pintar viewport central scrolleable (filas 7..=(rows - footer_rows - overlay_rows))
    let view_start = HUD_ROWS + 1;
    let view_end = rows.saturating_sub(footer_rows + overlay_rows);
    let view_height = view_end.saturating_sub(view_start - 1) as usize;
    let wrap_width = (cols as usize).saturating_sub(4).max(32);

    let all_lines = build_transcript_lines(&state.entries, state.show_thinking, wrap_width);
    let total = all_lines.len();
    let max_scroll = total.saturating_sub(view_height);
    state.last_max_scroll = max_scroll;
    if state.scroll_from_bottom > max_scroll {
        state.scroll_from_bottom = max_scroll;
    }

    let end_idx = total.saturating_sub(state.scroll_from_bottom);
    let start_idx = end_idx.saturating_sub(view_height);
    let visible = &all_lines[start_idx..end_idx];

    for offset in 0..view_height {
        let row = view_start + offset as u16;
        if let Some(line) = visible.get(offset) {
            let _ = write!(out, "\x1b[{};1H\x1b[2K{}\x1b[0m", row, line);
        } else {
            let _ = write!(out, "\x1b[{};1H\x1b[2K", row);
        }
    }

    if overlay_rows >= 3 {
        let overlay_start_row = view_end + 1;
        draw_model_picker_overlay(
            &mut out,
            state,
            &picker_candidates,
            cols,
            overlay_start_row,
            overlay_rows,
        );
    }

    // 3. Pintar barra inferior dinámica (divisor + 1..=3 filas de prompt + barra atajos)
    let cursor_pos_opt = draw_footer(&mut out, state, cols, rows, &footer_layout);

    let _ = write!(out, "\x1b[?7h");
    if let Some((cx, cy)) = cursor_pos_opt {
        let _ = write!(out, "\x1b[{};{}H\x1b[?25h", cy, cx);
    }
    let _ = out.flush();
}

fn has_following_sibling_action(entries: &[TranscriptItem], idx: usize, indent: usize) -> bool {
    for next in &entries[idx + 1..] {
        match next {
            TranscriptItem::UserPrompt(_) | TranscriptItem::GroupHeader { .. } | TranscriptItem::Card { .. } => {
                return false;
            }
            TranscriptItem::GroupFooter { indent: f_indent, .. } => {
                if *f_indent <= indent {
                    return false;
                }
            }
            TranscriptItem::Action { indent: a_indent, .. } => {
                if *a_indent < indent {
                    return false;
                }
                if *a_indent == indent {
                    return true;
                }
            }
            TranscriptItem::Thinking { indent: t_indent, .. }
            | TranscriptItem::Notice { indent: t_indent, .. } => {
                if *t_indent < indent {
                    return false;
                }
            }
        }
    }
    false
}

fn format_thinking_meta(turns: usize, first_tag: &str, last_tag: &str, chars: usize) -> String {
    if turns <= 1 {
        if first_tag.is_empty() {
            format!("{} chars", chars)
        } else {
            format!("{} · {} chars", first_tag, chars)
        }
    } else if !first_tag.is_empty() && !last_tag.is_empty() && first_tag != last_tag {
        format!("{}–{} · {} chars", first_tag, last_tag, chars)
    } else {
        format!("{} pasos · {} chars", turns, chars)
    }
}

fn build_transcript_lines(
    entries: &[TranscriptItem],
    show_thinking: bool,
    wrap_width: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    for (idx, item) in entries.iter().enumerate() {
        match item {
            TranscriptItem::UserPrompt(text) => {
                out.push(String::new());
                let wrapped = wrap_plain(text, wrap_width.saturating_sub(4));
                for (i, w) in wrapped.into_iter().enumerate() {
                    if i == 0 {
                        out.push(format!("\x1b[1;38;2;139;233;253m❯ {}\x1b[0m", w));
                    } else {
                        out.push(format!("  \x1b[38;2;248;248;242m{}\x1b[0m", w));
                    }
                }
            }
            TranscriptItem::GroupHeader { icon, title, meta } => {
                out.push(String::new());
                if meta.is_empty() {
                    out.push(format!(
                        "\x1b[38;2;139;233;253m╭─\x1b[1;38;2;248;248;242m {} {}\x1b[0m",
                        icon, title
                    ));
                } else {
                    out.push(format!(
                        "\x1b[38;2;139;233;253m╭─\x1b[1;38;2;248;248;242m {} {} \x1b[0;38;2;98;114;164m· \x1b[38;2;241;250;140m{}\x1b[0m",
                        icon, title, meta
                    ));
                }
            }
            TranscriptItem::Action {
                indent,
                verb,
                target,
                meta,
            } => {
                let branch = if has_following_sibling_action(entries, idx, *indent) {
                    "├─"
                } else {
                    "└─"
                };
                let guides = "│  ".repeat(*indent);
                let pad = format!("\x1b[38;2;139;233;253m│\x1b[0m  {}{} ", guides, branch);
                let max_target = wrap_width.saturating_sub(18 + indent * 3);
                let meta_part = if meta.is_empty() {
                    String::new()
                } else {
                    format!(" \x1b[38;2;98;114;164m({})\x1b[0m", meta)
                };
                out.push(format!(
                    "{}\x1b[1;38;2;80;250;123m{:<8}\x1b[0m \x1b[38;2;248;248;242m{}\x1b[0m{}",
                    pad,
                    verb,
                    truncate_chars(target, max_target.max(18)),
                    meta_part
                ));
            }
            TranscriptItem::Thinking {
                indent,
                actor,
                turns,
                first_tag,
                last_tag,
                chars,
                text,
            } => {
                let prefix = format!("\x1b[38;2;139;233;253m│\x1b[0m  {}", "│  ".repeat(*indent));
                let meta = format_thinking_meta(*turns, first_tag, last_tag, *chars);
                if !show_thinking {
                    out.push(format!(
                        "{}\x1b[38;2;98;114;164m🧠 {} ({} · Ctrl+O)\x1b[0m",
                        prefix, actor, meta
                    ));
                } else {
                    out.push(format!(
                        "{}\x1b[38;2;189;147;249m🧠 {} ({}):\x1b[0m",
                        prefix, actor, meta
                    ));
                    for w in wrap_plain(text, wrap_width.saturating_sub(8 + indent * 3)) {
                        out.push(format!(
                            "{}  \x1b[38;2;148;163;184m{}\x1b[0m",
                            prefix, w
                        ));
                    }
                }
            }
            TranscriptItem::GroupFooter {
                indent,
                icon,
                summary,
            } => {
                let prefix = if *indent == 0 {
                    "\x1b[38;2;139;233;253m╰─\x1b[0m".to_string()
                } else {
                    format!(
                        "\x1b[38;2;139;233;253m│\x1b[0m  {}╰─",
                        "│  ".repeat(*indent)
                    )
                };
                out.push(format!(
                    "{} \x1b[38;2;80;250;123m{} {}\x1b[0m",
                    prefix, icon, summary
                ));
            }
            TranscriptItem::Card {
                icon,
                title,
                meta,
                body,
            } => {
                out.push(String::new());
                let hdr = if meta.is_empty() {
                    format!(
                        "\x1b[38;2;80;250;123m╭─\x1b[1;38;2;248;248;242m {} {}\x1b[0m",
                        icon, title
                    )
                } else {
                    format!(
                        "\x1b[38;2;80;250;123m╭─\x1b[1;38;2;248;248;242m {} {} \x1b[0;38;2;98;114;164m· \x1b[38;2;241;250;140m{}\x1b[0m",
                        icon, title, meta
                    )
                };
                out.push(hdr);
                for line in wrap_plain(body, wrap_width.saturating_sub(4)) {
                    out.push(format!(
                        "\x1b[38;2;80;250;123m│\x1b[0m  {}",
                        line
                    ));
                }
                out.push("\x1b[38;2;80;250;123m╰────────────────────────────────────────────\x1b[0m".to_string());
            }
            TranscriptItem::Notice {
                indent,
                is_warn,
                text,
            } => {
                let color = if *is_warn {
                    "\x1b[38;2;255;184;108m"
                } else {
                    "\x1b[38;2;139;233;253m"
                };
                let prefix = format!("\x1b[38;2;139;233;253m│\x1b[0m  {}", "│  ".repeat(*indent));
                for w in wrap_plain(text, wrap_width.saturating_sub(6 + indent * 3)) {
                    out.push(format!("{}{}{}\x1b[0m", prefix, color, w));
                }
            }
        }
    }
    out
}

struct WrappedInputLine {
    text: String,
    start_char: usize,
    end_char: usize,
}

struct FooterInputLayout {
    visible_lines: Vec<String>,
    cursor_vis_row: usize,
    cursor_col: u16,
    total_rows: u16,
    show_prefix_on_row0: bool,
}

fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let code = c as u32;
            if code >= 0x1F300 || (0x2600..=0x27BF).contains(&code) {
                2
            } else {
                1
            }
        })
        .sum()
}

/// Envuelve el texto del prompt por palabras según ancho disponible de primera línea y líneas siguientes.
fn wrap_input_lines_with_cursor(
    text: &str,
    cursor_chars: usize,
    first_w: usize,
    cont_w: usize,
) -> (Vec<WrappedInputLine>, usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let cur = cursor_chars.min(chars.len());
    let first_budget = first_w.max(8);
    let cont_budget = cont_w.max(12);

    let mut lines: Vec<WrappedInputLine> = Vec::new();
    let mut pos = 0usize;

    while pos < chars.len() {
        let budget = if lines.is_empty() { first_budget } else { cont_budget };
        let remaining = chars.len() - pos;
        let end = if remaining <= budget {
            chars.len()
        } else {
            let limit = pos + budget;
            let mut split = limit;
            for i in (pos + 1..limit).rev() {
                if chars[i] == ' ' {
                    split = i + 1;
                    break;
                }
            }
            split
        };

        let slice: String = chars[pos..end].iter().collect();
        lines.push(WrappedInputLine {
            text: slice,
            start_char: pos,
            end_char: end,
        });
        pos = end;
    }

    if lines.is_empty() {
        lines.push(WrappedInputLine {
            text: String::new(),
            start_char: 0,
            end_char: 0,
        });
    }

    // Ubicar en qué línea visual cae `cursor_chars`
    let mut cur_line = lines.len() - 1;
    let mut cur_offset = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        let is_last = idx + 1 == lines.len();
        if cur < line.end_char || (is_last && cur <= line.end_char) {
            cur_line = idx;
            let local_chars = cur.saturating_sub(line.start_char);
            cur_offset = display_width(
                &line.text.chars().take(local_chars).collect::<String>(),
            );
            break;
        }
    }

    (lines, cur_line, cur_offset)
}

fn compute_footer_layout(state: &HudState, cols: u16) -> FooterInputLayout {
    if state.is_busy && !state.awaiting_clarify {
        return FooterInputLayout {
            visible_lines: vec![String::new()],
            cursor_vis_row: 0,
            cursor_col: 1,
            total_rows: FOOTER_ROWS,
            show_prefix_on_row0: false,
        };
    }

    let w = (cols as usize).max(40);
    let prefix_vis = display_width(&state.prompt_prefix) + 3; // "<prefix> > "
    let cont_prefix_vis = 2usize; // "  " en líneas 2 y 3
    let first_w = w.saturating_sub(prefix_vis + 1).max(8);
    let cont_w = w.saturating_sub(cont_prefix_vis + 1).max(16);

    let (all_lines, cur_line, cur_offset) =
        wrap_input_lines_with_cursor(&state.input_text, state.input_cursor_chars, first_w, cont_w);

    let vis_count = all_lines.len().clamp(1, MAX_INPUT_LINES);
    let max_start = all_lines.len().saturating_sub(vis_count);
    let win_start = if cur_line >= vis_count {
        (cur_line + 1 - vis_count).min(max_start)
    } else {
        0
    };
    let win_end = (win_start + vis_count).min(all_lines.len());
    let visible_lines: Vec<String> = all_lines[win_start..win_end]
        .iter()
        .map(|l| l.text.clone())
        .collect();
    let cursor_vis_row = cur_line.saturating_sub(win_start);
    let line_prefix_vis = if cur_line == 0 && win_start == 0 {
        prefix_vis
    } else {
        cont_prefix_vis
    };
    let cursor_col = (line_prefix_vis + 1 + cur_offset).min(cols as usize) as u16;

    FooterInputLayout {
        visible_lines,
        cursor_vis_row,
        cursor_col: cursor_col.max(1),
        total_rows: (2 + vis_count) as u16,
        show_prefix_on_row0: win_start == 0,
    }
}

fn draw_model_picker_overlay<W: Write>(
    out: &mut W,
    state: &HudState,
    candidates: &[String],
    cols: u16,
    start_row: u16,
    overlay_rows: u16,
) {
    let inner_w = (cols as usize).saturating_sub(2).max(36);
    let header_title = " 🧠 Selector `/model` (Tab: autocompletar · ↑↓: elegir · Enter: activar · Esc: cerrar) ";
    let title_clean = truncate_chars(header_title, inner_w.saturating_sub(2));
    let dash_len = inner_w.saturating_sub(display_width(&title_clean));
    let top_line = format!(
        "\x1b[38;2;139;233;253m╭─\x1b[1;38;2;241;250;140m{}\x1b[0;38;2;139;233;253m{}╮\x1b[0m",
        title_clean,
        "─".repeat(dash_len.saturating_sub(1))
    );
    let _ = write!(out, "\x1b[{};1H\x1b[2K{}", start_row, top_line);

    let list_slots = overlay_rows.saturating_sub(2) as usize;
    let total = candidates.len();
    let sel = state.model_picker_selected.min(total.saturating_sub(1));
    let win_start = if total <= list_slots {
        0
    } else {
        (sel.saturating_sub(list_slots / 2)).min(total - list_slots)
    };

    for idx in 0..list_slots {
        let row = start_row + 1 + idx as u16;
        if let Some(model) = candidates.get(win_start + idx) {
            let abs_idx = win_start + idx;
            let is_sel = abs_idx == sel;
            let is_active = if model == "manager" {
                !state.is_direct_mode
            } else {
                state.is_direct_mode && state.active_model == *model
            };
            let run_badge = state
                .model_picker_running
                .iter()
                .find(|(m, _)| m == model)
                .map(|(_, st)| format!(" [{}]", st))
                .unwrap_or_default();
            let role_badge = if model == "manager" {
                " (Orquestador ti_manager)"
            } else if is_active {
                " ◀ activo"
            } else {
                ""
            };
            let arrow = if is_sel { "▶" } else { " " };
            let text = truncate_chars(
                &format!("{} {}{}{}", arrow, model, run_badge, role_badge),
                inner_w.saturating_sub(2),
            );
            let styled = if is_sel {
                format!(
                    "\x1b[38;2;139;233;253m│\x1b[0m \x1b[1;38;2;80;250;123m{}\x1b[0m",
                    text
                )
            } else {
                format!(
                    "\x1b[38;2;139;233;253m│\x1b[0m \x1b[38;2;248;248;242m{}\x1b[0m",
                    text
                )
            };
            let _ = write!(out, "\x1b[{};1H\x1b[2K{}", row, styled);
        } else if total == 0 && idx == 0 {
            let _ = write!(
                out,
                "\x1b[{};1H\x1b[2K\x1b[38;2;139;233;253m│\x1b[0m \x1b[38;2;255;184;108m(Sin coincidencias para ese filtro)\x1b[0m",
                row
            );
        } else {
            let _ = write!(
                out,
                "\x1b[{};1H\x1b[2K\x1b[38;2;139;233;253m│\x1b[0m",
                row
            );
        }
    }

    let bottom_row = start_row + overlay_rows - 1;
    let _ = write!(
        out,
        "\x1b[{};1H\x1b[2K\x1b[38;2;139;233;253m╰{}╯\x1b[0m",
        bottom_row,
        "─".repeat(inner_w)
    );
}

fn draw_footer<W: Write>(
    out: &mut W,
    state: &HudState,
    cols: u16,
    rows: u16,
    layout: &FooterInputLayout,
) -> Option<(u16, u16)> {
    let w = (cols as usize).max(40);
    let div_row = rows - layout.total_rows + 1;
    let input_start_row = div_row + 1;
    let hint_row = rows;

    let think_badge = if state.show_thinking {
        "\x1b[38;2;80;250;123mCtrl+O: <think> ON\x1b[0m"
    } else {
        "\x1b[38;2;98;114;164mCtrl+O: <think> OFF\x1b[0m"
    };
    let scroll_badge = if state.scroll_from_bottom > 0 {
        format!(
            "\x1b[1;38;2;255;184;108m▲ +{} líneas (PgDn/End)\x1b[0m",
            state.scroll_from_bottom
        )
    } else {
        "\x1b[38;2;98;114;164mEn vivo ↓\x1b[0m".to_string()
    };
    let sep_len = w.saturating_sub(44).clamp(4, 60);
    let _ = write!(
        out,
        "\x1b[{};1H\x1b[2K\x1b[38;2;98;114;164m{}\x1b[0m {} \x1b[38;2;98;114;164m│\x1b[0m {}\x1b[0m",
        div_row,
        "─".repeat(sep_len),
        think_badge,
        scroll_badge
    );

    let cursor_pos = if state.is_busy && !state.awaiting_clarify {
        let msg = "\x1b[38;2;241;250;140m⏳ Ejecutando…\x1b[0m \x1b[38;2;98;114;164m(Rueda/PgUp: Scroll │ Ctrl+O: <think> │ Ctrl+C: Abortar)\x1b[0m";
        let _ = write!(out, "\x1b[{};1H\x1b[2K{}", input_start_row, msg);
        None
    } else {
        for (i, line_txt) in layout.visible_lines.iter().enumerate() {
            let row = input_start_row + i as u16;
            if i == 0 && layout.show_prefix_on_row0 {
                let _ = write!(
                    out,
                    "\x1b[{};1H\x1b[2K\x1b[1;38;2;139;233;253m{} \x1b[38;2;80;250;123m>\x1b[0m {}",
                    row, state.prompt_prefix, line_txt
                );
            } else {
                let _ = write!(out, "\x1b[{};1H\x1b[2K  {}", row, line_txt);
            }
        }
        Some((layout.cursor_col, input_start_row + layout.cursor_vis_row as u16))
    };

    let hint_text = if state.model_picker_open && !state.is_busy {
        "Tab: Autocompletar │ ↑↓: Elegir modelo │ Enter: Activar │ Esc: Cerrar"
    } else {
        "Enter: Enviar │ @agente <tarea> │ /help │ /model │ /plan │ /resume │ /think │ /exit"
    };
    let _ = write!(
        out,
        "\x1b[{};1H\x1b[2K\x1b[38;2;98;114;164m{}\x1b[0m",
        hint_row, hint_text
    );

    cursor_pos
}

fn draw_hud_at_top<W: Write>(out: &mut W, state: &HudState, cols: u16) {
    let w = (cols as usize).saturating_sub(2).max(40);

    let ep_short = state
        .endpoint
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches("/v1");
    let srv_badge = match state.server_online {
        Some(true) => format!("● {} ({} mod)", ep_short, state.available_models_count),
        Some(false) => format!("○ {} OFFLINE", ep_short),
        None => format!("◌ {}", ep_short),
    };
    let srv_color = match state.server_online {
        Some(true) => "\x1b[38;2;80;250;123m",
        Some(false) => "\x1b[38;2;255;85;85m",
        None => "\x1b[38;2;241;250;140m",
    };

    let ws_short = truncate_chars(&state.workspace_name, 22);
    let top_dash_len = w.saturating_sub(36 + ws_short.chars().count() + srv_badge.chars().count());
    let line1 = format!(
        "\x1b[38;2;139;233;253m╭─\x1b[1;38;2;255;184;108m ⚡ OPENED-AI v0.1 \x1b[0;38;2;98;114;164m── \x1b[38;2;248;248;242m📁 {} \x1b[38;2;98;114;164m{} \x1b[0m{}{}\x1b[0m",
        ws_short,
        "─".repeat(top_dash_len.max(2)),
        srv_color,
        srv_badge
    );

    let mode_color = if state.is_direct_mode {
        "\x1b[1;38;2;255;184;108m"
    } else {
        "\x1b[1;38;2;80;250;123m"
    };
    let vel_str = if state.last_tps > 0.0 {
        format!("{:.1} t/s", state.last_tps)
    } else {
        "— t/s".to_string()
    };
    let line2 = format!(
        "\x1b[38;2;139;233;253m│\x1b[0m {}Modo:\x1b[0m {:<36} \x1b[38;2;98;114;164m│\x1b[0m \x1b[38;2;189;147;249mModelo:\x1b[0m \x1b[1m{:<18}\x1b[0m \x1b[38;2;98;114;164m│\x1b[0m \x1b[38;2;139;233;253mVel:\x1b[0m {}",
        mode_color,
        truncate_chars(&state.mode_label, 36),
        truncate_chars(&state.active_model, 18),
        vel_str
    );

    let phase_bar = if state.plan_total > 0 {
        let pct = (state.plan_done * 100) / state.plan_total;
        let filled = (state.plan_done * 14) / state.plan_total;
        format!(
            "\x1b[38;2;80;250;123m{}\x1b[38;2;68;71;90m{}\x1b[0m {:>3}% ({}/{})",
            "█".repeat(filled),
            "░".repeat(14usize.saturating_sub(filled)),
            pct,
            state.plan_done,
            state.plan_total
        )
    } else {
        "\x1b[38;2;98;114;164m░░░░░░░░░░░░░░   —%\x1b[0m".to_string()
    };
    let phase_budget = w.saturating_sub(36);
    let line3 = format!(
        "\x1b[38;2;139;233;253m│\x1b[0m \x1b[38;2;241;250;140mFase:\x1b[0m {} \x1b[38;2;98;114;164m•\x1b[0m {}",
        phase_bar,
        truncate_chars(&state.current_phase, phase_budget.max(16))
    );

    let kv_max = state.kv_max.max(1);
    let kv_pct = ((state.kv_used as f64 / kv_max as f64) * 100.0).min(100.0);
    let kv_filled = ((state.kv_used * 14) / kv_max).min(14);
    let kv_color = if state.kv_used >= state.kv_soft_limit && state.kv_soft_limit > 0 {
        "\x1b[38;2;255;85;85m"
    } else if state.kv_soft_limit > 0 && state.kv_used * 100 >= state.kv_soft_limit * 70 {
        "\x1b[38;2;241;250;140m"
    } else {
        "\x1b[38;2;80;250;123m"
    };
    let line4 = format!(
        "\x1b[38;2;139;233;253m│\x1b[0m \x1b[38;2;139;233;253mKV:  \x1b[0m {}{}\x1b[38;2;68;71;90m{}\x1b[0m {:>5}/{:<5} tk ({:>4.1}%) \x1b[38;2;98;114;164m│\x1b[0m Soft: {} \x1b[38;2;98;114;164m│\x1b[0m \x1b[38;2;80;250;123mAhorro: {} tk\x1b[0m",
        kv_color,
        "█".repeat(kv_filled),
        "░".repeat(14usize.saturating_sub(kv_filled)),
        state.kv_used,
        kv_max,
        kv_pct,
        state.kv_soft_limit,
        format_compact_k(state.kv_saved)
    );

    let act_budget = w.saturating_sub(42);
    let line5 = format!(
        "\x1b[38;2;139;233;253m│\x1b[0m \x1b[38;2;255;121;198mAct:\x1b[0m  {:<width$} \x1b[38;2;98;114;164m│\x1b[0m Subagentes: \x1b[1m{}\x1b[0m \x1b[38;2;98;114;164m│ /help • /model\x1b[0m",
        truncate_chars(&state.activity, act_budget.max(16)),
        state.subagents_count,
        width = act_budget.clamp(16, 44)
    );

    let line6 = format!("\x1b[38;2;139;233;253m╰{}╯\x1b[0m", "─".repeat(w));

    let rows = [line1, line2, line3, line4, line5, line6];
    for (idx, content) in rows.iter().enumerate() {
        let _ = write!(out, "\x1b[{};1H\x1b[2K{}\x1b[0m", idx + 1, content);
    }
}

fn wrap_plain(text: &str, max_width: usize) -> Vec<String> {
    let w = max_width.max(20);
    let mut lines = Vec::new();
    for raw_line in text.replace('\t', "    ").lines() {
        if raw_line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut count = 0usize;
        for ch in raw_line.chars() {
            current.push(ch);
            count += 1;
            if count >= w {
                lines.push(std::mem::take(&mut current));
                count = 0;
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn format_compact_k(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let clean = s.replace(['\r', '\n'], " ");
    let count = clean.chars().count();
    if count <= max_chars || max_chars <= 1 {
        clean
    } else {
        let mut out: String = clean.chars().take(max_chars - 1).collect();
        out.push('…');
        out
    }
}

#[cfg(windows)]
fn enable_vt_support() {
    const STD_OUTPUT_HANDLE: i32 = -11;
    const STD_ERROR_HANDLE: i32 = -12;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    extern "system" {
        fn GetStdHandle(nStdHandle: i32) -> isize;
        fn GetConsoleMode(hConsoleHandle: isize, lpMode: *mut u32) -> i32;
        fn SetConsoleMode(hConsoleHandle: isize, dwMode: u32) -> i32;
    }

    unsafe {
        for std_id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let handle = GetStdHandle(std_id);
            if handle != -1 && handle != 0 {
                let mut mode: u32 = 0;
                if GetConsoleMode(handle, &mut mode) != 0 {
                    let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
                }
            }
        }
    }
}

#[cfg(not(windows))]
fn enable_vt_support() {}

pub fn terminal_size() -> (u16, u16) {
    terminal::size().unwrap_or((100, 30))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tool_target_extracts_clean_targets() {
        let (v, t) = format_tool_target("read_file", r#"{"path":"src/main.rs","start_line":1,"end_line":80}"#);
        assert_eq!(v, "Read");
        assert_eq!(t, "src/main.rs #L1-80");

        let (v2, t2) = format_tool_target("write_file", r#"{"path":"a.rs","content":"hello"}"#);
        assert_eq!(v2, "Write");
        assert_eq!(t2, "a.rs (5 B)");
    }

    #[test]
    fn parse_plan_prefers_checkboxes_and_cleans_tables() {
        let dir = std::env::temp_dir().join("lh_hud_plan_test");
        let harness_dir = dir.join(".harness");
        let _ = fs::create_dir_all(&harness_dir);
        fs::write(
            harness_dir.join("PLAN.md"),
            "| Fase | Nombre | Estado |\n|---|---|---|\n| 0 | Init | ✅ |\n| 1 | Relay SSE | 🔄 semi_sr_dev |\n| 2 | API REST | ⬜ semi_sr_dev |\n",
        )
        .unwrap();

        let mut st = HudState {
            workspace_path: dir.clone(),
            ..Default::default()
        };
        refresh_plan_fields(&mut st);
        assert_eq!(st.plan_done, 1);
        assert_eq!(st.plan_total, 3);
        assert!(!st.current_phase.contains('|'));
        assert!(st.current_phase.contains("Fase 1: Relay SSE"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrap_input_expands_to_two_or_three_visible_lines_without_cutting_words() {
        let mut st = HudState::default();
        st.input_text = "Quiero analizar este repositorio completo y generar un diagrama detallado de sus componentes".to_string();
        st.input_cursor_chars = st.input_text.chars().count();

        let layout = compute_footer_layout(&st, 52);
        assert_eq!(layout.visible_lines.len(), 3);
        assert_eq!(layout.total_rows, 5);
        assert!(layout.show_prefix_on_row0);
        assert_eq!(layout.cursor_vis_row, 2);
    }

    #[test]
    fn model_picker_filters_by_prefix_and_substring() {
        let st = HudState {
            model_picker_models: vec![
                "gemma4-26b".to_string(),
                "qwen36-native".to_string(),
                "qwen38-distill".to_string(),
                "qwen38-uncensored".to_string(),
            ],
            input_text: "/model qwen38".to_string(),
            ..Default::default()
        };
        let filtered = filtered_model_picker_candidates(&st);
        assert_eq!(
            filtered,
            vec![
                "qwen38-distill".to_string(),
                "qwen38-uncensored".to_string(),
            ]
        );
    }

    fn strip_ansi_codes(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(nc) = chars.next() {
                        if ('@'..='~').contains(&nc) {
                            break;
                        }
                    }
                    continue;
                }
            }
            out.push(c);
        }
        out
    }

    fn push_sim_think(entries: &mut Vec<TranscriptItem>, indent: usize, actor: &str, text: &str) {
        let chars = text.chars().count();
        let (actor_base, turn_tag) = split_actor_turn(actor);
        if !coalesce_thinking_locked(entries, indent, &actor_base, turn_tag.as_deref(), chars, text) {
            let formatted = match turn_tag.as_deref() {
                Some(tag) => format!("[{}] {}", tag, text),
                None => text.to_string(),
            };
            entries.push(TranscriptItem::Thinking {
                indent,
                actor: actor_base,
                turns: 1,
                first_tag: turn_tag.clone().unwrap_or_default(),
                last_tag: turn_tag.unwrap_or_default(),
                chars,
                text: formatted,
            });
        }
    }

    #[test]
    fn groups_same_agent_iterations_minimally_without_cloud_icons() {
        let mut entries: Vec<TranscriptItem> = Vec::new();
        entries.push(TranscriptItem::GroupHeader {
            icon: "👔".into(),
            title: "TI MANAGER".into(),
            meta: "qwen38-distill · turnos ∞".into(),
        });

        push_sim_think(&mut entries, 0, "TI Manager t7", &"a".repeat(488));
        entries.push(TranscriptItem::Action {
            indent: 0,
            verb: "Plan".into(),
            target: "Actualizar .harness/PLAN.md".into(),
            meta: "turno 7".into(),
        });
        push_sim_think(&mut entries, 0, "TI Manager t8", &"b".repeat(1493));
        entries.push(TranscriptItem::Action {
            indent: 0,
            verb: "Agent".into(),
            target: "@semi_sr_dev (qwen38-distill)".into(),
            meta: "Implementar FASE 1 (Capa de Datos)".into(),
        });

        entries.push(TranscriptItem::Notice {
            indent: 1,
            is_warn: false,
            text: "📚 skills · red-auth-jwt-audit, red-infra-secrets-hardening".into(),
        });
        push_sim_think(&mut entries, 1, "semi_sr_dev", &"c".repeat(288));
        entries.push(TranscriptItem::Action {
            indent: 1,
            verb: "Run".into(),
            target: "cd /home/axel/GoogleDrive/harness_to_linux/UIDE/go_project1 && find . -maxdepth 2".into(),
            meta: "paso 1/18".into(),
        });
        push_sim_think(&mut entries, 1, "semi_sr_dev", &"d".repeat(100));
        entries.push(TranscriptItem::Action {
            indent: 1,
            verb: "Read".into(),
            target: "/home/axel/GoogleDrive/harness_to_linux/UIDE/go_project1/go.mod".into(),
            meta: "paso 2/18".into(),
        });
        push_sim_think(&mut entries, 1, "semi_sr_dev", &"e".repeat(483));
        entries.push(TranscriptItem::Action {
            indent: 1,
            verb: "Run".into(),
            target: "mkdir -p migrations".into(),
            meta: "paso 3/18".into(),
        });
        push_sim_think(&mut entries, 1, "semi_sr_dev", &"f".repeat(73));
        entries.push(TranscriptItem::Action {
            indent: 1,
            verb: "Write".into(),
            target: "/home/axel/GoogleDrive/harness_to_linux/UIDE/go_project1/migrations/001_init.sql (2078 B)".into(),
            meta: "paso 4/18".into(),
        });
        entries.push(TranscriptItem::GroupFooter {
            indent: 1,
            icon: "✅".into(),
            summary: "@semi_sr_dev · 4 pasos · 12.4s · 31.2 t/s · +3.4k tk ahorrados".into(),
        });

        push_sim_think(&mut entries, 0, "TI Manager t9", &"g".repeat(220));
        entries.push(TranscriptItem::Action {
            indent: 0,
            verb: "Plan".into(),
            target: "Marcar Fase 1 completada en .harness/PLAN.md".into(),
            meta: "turno 9".into(),
        });
        entries.push(TranscriptItem::GroupFooter {
            indent: 0,
            icon: "✅".into(),
            summary: "Completado · 9 turnos · 38.1s · 30.8 t/s · 1 subagente · +3.4k tk ahorrados".into(),
        });

        let collapsed: Vec<String> = build_transcript_lines(&entries, false, 118)
            .into_iter()
            .map(|l| strip_ansi_codes(&l))
            .collect();
        let joined = collapsed.join("\n");
        println!("\n=== SIMULACION OUTPUT MINIMALISTA AGRUPADO ===\n{}\n=============================================", joined);

        assert!(!joined.contains('💭'));
        assert!(joined.contains("📚 skills ·"));
        assert!(joined.contains("🧠 TI Manager (t7–t8 · 1981 chars · Ctrl+O)"));
        assert!(joined.contains("🧠 semi_sr_dev (4 pasos · 944 chars · Ctrl+O)"));
        assert_eq!(joined.matches("🧠 semi_sr_dev").count(), 1);
        assert!(joined.contains("│  ├─ Run"));
        assert!(joined.contains("│  ├─ Read"));
        assert!(joined.contains("│  └─ Write"));
        assert!(joined.contains("╰─ ✅ @semi_sr_dev"));
        assert!(joined.contains("🧠 TI Manager (t9 · 220 chars · Ctrl+O)"));
    }
}
