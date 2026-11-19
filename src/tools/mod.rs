pub mod bg_process;
pub mod diagnostics;
pub mod vision_cdp;
pub mod web_rag;

use crate::core::checkpoint::create_git_checkpoint;
use crate::interfaces::tui_hud;
use crate::skills::{format_skills_catalog, load_skill_body};
use regex::Regex;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

/// Devuelve los esquemas JSON planos y compactos (para evitar bloqueos GBNF en ik_llama.cpp)
/// filtrados únicamente por las herramientas permitidas para el agente actual.
pub fn get_tools_schema(allowed_tools: &[String]) -> Vec<Value> {
    let mut schemas = Vec::new();

    for tool in allowed_tools {
        let schema = match tool.as_str() {
            "delegate_subagent" => json!({
                "type": "function",
                "function": {
                    "name": "delegate_subagent",
                    "description": "Invoca secuencialmente a un subagente especializado con contexto desechable.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "agent_name": {
                                "type": "string",
                                "description": "Nombre del subagente en config.yaml (ej. explorer, software_architect, semi_sr_dev, qa_tester, web_researcher, frontend_inspector, security_auditor)"
                            },
                            "task_brief": {
                                "type": "string",
                                "description": "Instrucción clara y autocontenida de lo que debe hacer y entregar"
                            },
                            "context_hints": {
                                "type": "string",
                                "description": "Rutas de archivos o contexto relevante opcional"
                            }
                        },
                        "required": ["agent_name", "task_brief"]
                    }
                }
            }),
            "update_plan" => json!({
                "type": "function",
                "function": {
                    "name": "update_plan",
                    "description": "Actualiza el archivo .harness/PLAN.md con el progreso de tareas.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "plan_markdown": {
                                "type": "string",
                                "description": "Contenido Markdown actualizado del plan"
                            }
                        },
                        "required": ["plan_markdown"]
                    }
                }
            }),
            "list_files" => json!({
                "type": "function",
                "function": {
                    "name": "list_files",
                    "description": "Lista archivos del directorio de trabajo ignorando .git, target y node_modules.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "subdir": {
                                "type": "string",
                                "description": "Subdirectorio relativo opcional (por defecto '.')"
                            }
                        }
                    }
                }
            }),
            "read_file" => json!({
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Lee un archivo con números de línea en un rango acotado.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Ruta relativa o absoluta del archivo" },
                            "start_line": { "type": "integer", "description": "Línea inicial (1-based, default 1)" },
                            "end_line": { "type": "integer", "description": "Línea final (default 200)" }
                        },
                        "required": ["path"]
                    }
                }
            }),
            "write_file" => json!({
                "type": "function",
                "function": {
                    "name": "write_file",
                    "description": "Crea o sobrescribe un archivo completo.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Ruta del archivo" },
                            "content": { "type": "string", "description": "Contenido completo a escribir" }
                        },
                        "required": ["path", "content"]
                    }
                }
            }),
            "apply_diff" => json!({
                "type": "function",
                "function": {
                    "name": "apply_diff",
                    "description": "Edita quirúrgicamente un archivo reemplazando el bloque `search` por `replace`. Puedes emitir de 2 a 4 llamadas `apply_diff` en un mismo turno para cambios cortos, independientes y no solapados en el mismo archivo.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Ruta del archivo a modificar" },
                            "search": { "type": "string", "description": "Bloque exacto de líneas existentes a buscar" },
                            "replace": { "type": "string", "description": "Nuevo bloque de líneas que reemplazará a search" }
                        },
                        "required": ["path", "search", "replace"]
                    }
                }
            }),
            "search_code" => json!({
                "type": "function",
                "function": {
                    "name": "search_code",
                    "description": "Busca texto o patrón en archivos del repositorio, con filtros opcionales por subcarpeta y extensión/glob.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string", "description": "Texto a buscar dentro de los archivos" },
                            "subdir": { "type": "string", "description": "Subdirectorio relativo opcional (ej. 'src' o 'cowork_mobile')" },
                            "glob": { "type": "string", "description": "Patrón opcional de nombre/ruta (ej. '*.rs', '*.ts', '*auth*')" }
                        },
                        "required": ["query"]
                    }
                }
            }),
            "find_files" => json!({
                "type": "function",
                "function": {
                    "name": "find_files",
                    "description": "Encuentra archivos por nombre o patrón glob (ej. '*.rs', '*config*') en todo el repositorio.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "pattern": { "type": "string", "description": "Patrón glob o subcadena del archivo/ruta (ej. '*.rs', 'SKILL.md', '*relay*')" },
                            "subdir": { "type": "string", "description": "Subdirectorio relativo opcional (default '.')" }
                        },
                        "required": ["pattern"]
                    }
                }
            }),
            "project_memory" => json!({
                "type": "function",
                "function": {
                    "name": "project_memory",
                    "description": "Lee o actualiza la memoria persistente del proyecto (.harness/MEMORY.md) para recordar convenciones entre sesiones.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "action": { "type": "string", "description": "Operación: 'read', 'append' o 'replace'" },
                            "content": { "type": "string", "description": "Nota breve y factual a guardar (para 'append' o 'replace')" }
                        },
                        "required": ["action"]
                    }
                }
            }),
            "clarify" => json!({
                "type": "function",
                "function": {
                    "name": "clarify",
                    "description": "Pausa y solicita una aclaración breve al usuario (con opciones sugeridas) antes de tomar una decisión ambigua.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string", "description": "Pregunta puntual y concisa para el usuario" },
                            "options": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Lista corta opcional de opciones recomendadas (2-4)"
                            }
                        },
                        "required": ["question"]
                    }
                }
            }),
            "shell_exec" => json!({
                "type": "function",
                "function": {
                    "name": "shell_exec",
                    "description": if cfg!(target_os = "windows") {
                        "Ejecuta un comando en Windows PowerShell (usa ';' para encadenar, NO '&&') con truncamiento inteligente de logs."
                    } else {
                        "Ejecuta un comando en shell Unix/Linux (sh -c) con truncamiento inteligente de logs."
                    },
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "command": { "type": "string", "description": "Comando a ejecutar en el shell nativo del sistema" }
                        },
                        "required": ["command"]
                    }
                }
            }),
            "git_checkpoint" => json!({
                "type": "function",
                "function": {
                    "name": "git_checkpoint",
                    "description": "Crea un punto de restauración en Git antes de cambios críticos.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "label": { "type": "string", "description": "Etiqueta descriptiva del checkpoint" }
                        },
                        "required": ["label"]
                    }
                }
            }),
            "bg_start" => json!({
                "type": "function",
                "function": {
                    "name": "bg_start",
                    "description": "Inicia un servidor o proceso en segundo plano sin bloquear la terminal (redirige salida a .harness/bg_logs/).",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "description": "Identificador corto del proceso (ej. web_server, dev_app)" },
                            "command": { "type": "string", "description": "Comando a ejecutar en segundo plano" }
                        },
                        "required": ["name", "command"]
                    }
                }
            }),
            "bg_logs" => json!({
                "type": "function",
                "function": {
                    "name": "bg_logs",
                    "description": "Consulta las últimas líneas del log de un proceso en segundo plano iniciado con bg_start.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "description": "Identificador del proceso" },
                            "tail_lines": { "type": "integer", "description": "Cantidad de líneas finales a leer (default 50)" }
                        },
                        "required": ["name"]
                    }
                }
            }),
            "bg_kill" => json!({
                "type": "function",
                "function": {
                    "name": "bg_kill",
                    "description": "Detiene un proceso en segundo plano por su nombre o PID.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "description": "Identificador del proceso a terminar" },
                            "pid": { "type": "integer", "description": "PID opcional del proceso" }
                        },
                        "required": ["name"]
                    }
                }
            }),
            "list_skills" => json!({
                "type": "function",
                "function": {
                    "name": "list_skills",
                    "description": "Lista el catálogo compacto de Skills especializadas disponibles (SKILL.md).",
                    "parameters": {
                        "type": "object",
                        "properties": {}
                    }
                }
            }),
            "load_skill" => json!({
                "type": "function",
                "function": {
                    "name": "load_skill",
                    "description": "Carga bajo demanda el contenido completo de un archivo SKILL.md en el contexto actual.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "skill_name": { "type": "string", "description": "Nombre de la skill a cargar (ej. rust-clean-arch, owasp-deep-audit, frontend-responsive-ui, homelab-moe-tuning)" }
                        },
                        "required": ["skill_name"]
                    }
                }
            }),
            "web_rag" => json!({
                "type": "function",
                "function": {
                    "name": "web_rag",
                    "description": "Realiza búsquedas técnicas en la web (DuckDuckGo HTML) o extrae documentación limpia en Markdown desde una URL.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string", "description": "Consulta técnica a buscar en la web" },
                            "url": { "type": "string", "description": "URL directa opcional para extraer su documentación en Markdown" },
                            "max_chars": { "type": "integer", "description": "Límite máximo de caracteres (default 5000)" }
                        }
                    }
                }
            }),
            "vision_inspect" => json!({
                "type": "function",
                "function": {
                    "name": "vision_inspect",
                    "description": "Captura pantallas multi-viewport (mobile, tablet, desktop) de una URL o archivo .html con Chrome/Edge Headless (o lee una imagen .png/.jpg) y ejecuta análisis visual multimodal + telemetría DOM.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "target": { "type": "string", "description": "URL (http://...), ruta a archivo .html local o ruta a imagen .png/.jpg" },
                            "viewport": { "type": "string", "description": "Viewport de captura: desktop (1440x900), tablet (768x1024) o mobile (375x812)" },
                            "prompt": { "type": "string", "description": "Pregunta o aspecto visual específico a inspeccionar" },
                            "vision_model": { "type": "string", "description": "Modelo con proyector mmproj en llama-swap (default: qwen36-uncensored)" }
                        },
                        "required": ["target"]
                    }
                }
            }),
            "git_diff" => json!({
                "type": "function",
                "function": {
                    "name": "git_diff",
                    "description": "Inspecciona el estado actual de Git (`git status --short` y `git diff`) para auto-revisión o auditoría de cambios.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Ruta opcional de un archivo específico para ver su diff" }
                        }
                    }
                }
            }),
            _ => continue,
        };
        schemas.push(schema);
    }

    schemas
}

fn resolve_path(workspace: &Path, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        return p;
    }

    let joined = workspace.join(&p);
    if joined.exists() {
        return joined;
    }

    if let Some(ws_name) = workspace.file_name().and_then(|n| n.to_str()) {
        let trimmed = raw.trim_start_matches(['.', '/', '\\']);
        let stripped = trimmed
            .strip_prefix(&format!("{}/", ws_name))
            .or_else(|| trimmed.strip_prefix(&format!("{}\\", ws_name)));

        if let Some(rel) = stripped.filter(|s| !s.is_empty()) {
            let candidate = workspace.join(rel);
            if candidate.exists()
                || candidate
                    .parent()
                    .is_some_and(|parent| parent != workspace && parent.exists())
                || Path::new(rel).parent().is_some_and(|p| p.as_os_str().is_empty())
            {
                return candidate;
            }
        }
    }

    joined
}

/// Ejecuta una herramienta local y devuelve su salida compactada lista para el LLM
pub async fn execute_local_tool(
    tool_name: &str,
    raw_args: &str,
    workspace: &Path,
    max_shell_chars: usize,
    server_base_url: &str,
) -> String {
    let args: Value = serde_json::from_str(raw_args).unwrap_or_else(|_| json!({}));

    match tool_name {
        "update_plan" => {
            let content = args["plan_markdown"].as_str().unwrap_or("");
            let harness_dir = workspace.join(".harness");
            let _ = fs::create_dir_all(&harness_dir);
            let plan_file = harness_dir.join("PLAN.md");
            match fs::write(&plan_file, content) {
                Ok(_) => format!("[OK] Plan actualizado en {}", plan_file.display()),
                Err(e) => format!("[ERROR escribiendo PLAN.md]: {}", e),
            }
        }
        "list_files" => {
            let sub = args["subdir"].as_str().unwrap_or(".");
            let mut files = Vec::new();
            for entry in walk_workspace_files(workspace, sub, 4) {
                let rel = rel_display(workspace, entry.path());
                files.push(rel);
                if files.len() >= 150 {
                    files.push("... (truncado a 150 archivos)".to_string());
                    break;
                }
            }
            if files.is_empty() {
                "No se encontraron archivos.".to_string()
            } else {
                files.join("\n")
            }
        }
        "find_files" => {
            let pattern = args["pattern"].as_str().unwrap_or("*").trim();
            let sub = args["subdir"].as_str().unwrap_or(".");
            let mut files = Vec::new();
            for entry in walk_workspace_files(workspace, sub, 8) {
                let rel = rel_display(workspace, entry.path());
                if glob_matches(pattern, &rel) {
                    files.push(rel);
                    if files.len() >= 150 {
                        files.push("... (truncado a 150 resultados)".to_string());
                        break;
                    }
                }
            }
            if files.is_empty() {
                format!("No se encontraron archivos para '{}'", pattern)
            } else {
                files.join("\n")
            }
        }
        "read_file" => {
            let Some(p_str) = args["path"].as_str() else {
                return "[ERROR]: Falta el parámetro 'path'".to_string();
            };
            let start = args["start_line"].as_u64().unwrap_or(1).max(1) as usize;
            let end = args["end_line"].as_u64().unwrap_or((start + 180) as u64) as usize;
            let file_path = resolve_path(workspace, p_str);

            match fs::read_to_string(&file_path) {
                Ok(text) => {
                    let lines: Vec<&str> = text.lines().collect();
                    let total = lines.len();
                    let s_idx = (start - 1).min(total);
                    let e_idx = end.min(total);
                    let mut out = format!(
                        "Archivo: {} (Mostrando líneas {}..{} de {})\n",
                        file_path.display(),
                        s_idx + 1,
                        e_idx,
                        total
                    );
                    for (i, line) in lines[s_idx..e_idx].iter().enumerate() {
                        out.push_str(&format!("{:4} | {}\n", s_idx + i + 1, line));
                    }
                    out
                }
                Err(e) => format!("[ERROR leyendo {}]: {}", file_path.display(), e),
            }
        }
        "write_file" => {
            let Some(p_str) = args["path"].as_str() else {
                return "[ERROR]: Falta el parámetro 'path'".to_string();
            };
            let content = args["content"].as_str().unwrap_or("");
            let file_path = resolve_path(workspace, p_str);
            if let Some(parent) = file_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::write(&file_path, content) {
                Ok(_) => {
                    let diag = diagnostics::run_post_edit_diagnostics(workspace, &file_path);
                    format!(
                        "[OK] Archivo escrito exitosamente: {} ({} bytes)\n{}",
                        file_path.display(),
                        content.len(),
                        diag
                    )
                }
                Err(e) => format!("[ERROR escribiendo {}]: {}", file_path.display(), e),
            }
        }
        "apply_diff" => {
            let Some(p_str) = args["path"].as_str() else {
                return "[ERROR]: Falta el parámetro 'path'".to_string();
            };
            let search = args["search"].as_str().unwrap_or("");
            let replace = args["replace"].as_str().unwrap_or("");
            let file_path = resolve_path(workspace, p_str);

            let Ok(original) = fs::read_to_string(&file_path) else {
                return format!("[ERROR]: No se pudo abrir {}", file_path.display());
            };

            // Normalizar saltos de línea CRLF/LF para evitar fallos en Windows/Linux
            let norm_orig = original.replace("\r\n", "\n");
            let norm_search = search.replace("\r\n", "\n");
            let norm_replace = replace.replace("\r\n", "\n");

            if !norm_orig.contains(&norm_search) {
                let snippet_hint = find_closest_snippet(&norm_orig, &norm_search)
                    .map(|snippet| {
                        format!(
                            "\nFragmento más cercano en el archivo actual:\n{}",
                            snippet
                        )
                    })
                    .unwrap_or_default();
                return format!(
                    "[ERROR apply_diff]: El bloque `search` no coincide exactamente en {}. Verifica indentación/contexto antes de reintentar.{}",
                    file_path.display(),
                    snippet_hint
                );
            }

            let updated = norm_orig.replacen(&norm_search, &norm_replace, 1);
            match fs::write(&file_path, &updated) {
                Ok(_) => {
                    let diag = diagnostics::run_post_edit_diagnostics(workspace, &file_path);
                    format!(
                        "[OK] Diff aplicado quirúrgicamente en {}\n{}",
                        file_path.display(),
                        diag
                    )
                }
                Err(e) => format!("[ERROR guardando diff en {}]: {}", file_path.display(), e),
            }
        }
        "search_code" => {
            let Some(q) = args["query"].as_str() else {
                return "[ERROR]: Falta 'query'".to_string();
            };
            let sub = args["subdir"].as_str().unwrap_or(".");
            let glob = args["glob"].as_str().unwrap_or("").trim();
            let mut matches = Vec::new();
            for entry in walk_workspace_files(workspace, sub, 6) {
                let rel = rel_display(workspace, entry.path());
                if !glob.is_empty() && !glob_matches(glob, &rel) {
                    continue;
                }
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    for (idx, line) in content.lines().enumerate() {
                        if line.contains(q) {
                            matches.push(format!("{}:{}: {}", rel, idx + 1, line.trim()));
                            if matches.len() >= 50 {
                                break;
                            }
                        }
                    }
                }
                if matches.len() >= 50 {
                    break;
                }
            }
            if matches.is_empty() {
                format!("Sin coincidencias para '{}'", q)
            } else {
                matches.join("\n")
            }
        }
        "shell_exec" => {
            let Some(cmd_str) = args["command"].as_str() else {
                return "[ERROR]: Falta 'command'".to_string();
            };
            let output = if cfg!(target_os = "windows") {
                Command::new("powershell")
                    .args(["-NoProfile", "-Command", cmd_str])
                    .current_dir(workspace)
                    .output()
            } else {
                Command::new("sh")
                    .args(["-c", cmd_str])
                    .current_dir(workspace)
                    .output()
            };

            match output {
                Ok(o) => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    let combined = format!(
                        "Exit Code: {}\nSTDOUT:\n{}\nSTDERR:\n{}",
                        o.status.code().unwrap_or(-1),
                        stdout.trim(),
                        stderr.trim()
                    );
                    compact_log_output(&combined, max_shell_chars)
                }
                Err(e) => format!("[ERROR ejecutando comando]: {}", e),
            }
        }
        "git_checkpoint" => {
            let label = args["label"].as_str().unwrap_or("manual-checkpoint");
            match create_git_checkpoint(workspace, label) {
                Ok(Some(hash)) => format!("[OK] Git Checkpoint creado: {}", hash),
                Ok(None) => "[INFO] No se creó checkpoint (no es un repo git o sin commits)".to_string(),
                Err(e) => format!("[ERROR git_checkpoint]: {}", e),
            }
        }
        "bg_start" => bg_process::tool_bg_start(&args, workspace).await,
        "bg_logs" => bg_process::tool_bg_logs(&args, workspace),
        "bg_kill" => bg_process::tool_bg_kill(&args, workspace),
        "list_skills" => format_skills_catalog(workspace),
        "load_skill" => {
            let skill_name = args["skill_name"].as_str().unwrap_or("");
            load_skill_body(workspace, skill_name)
        }
        "web_rag" => web_rag::tool_web_rag(&args).await,
        "vision_inspect" => {
            vision_cdp::tool_vision_inspect(&args, workspace, server_base_url).await
        }
        "project_memory" => handle_project_memory(workspace, &args),
        "clarify" => {
            let question = args["question"].as_str().unwrap_or("").trim();
            if question.is_empty() {
                return "[ERROR]: Falta el parámetro 'question'".to_string();
            }
            let options: Vec<String> = args["options"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                        .filter(|s| !s.is_empty())
                        .take(6)
                        .collect()
                })
                .unwrap_or_default();
            tui_hud::request_clarification(question, &options).await
        }
        "git_diff" => {
            let file_filter = args["path"].as_str().unwrap_or("").trim();
            let status_out = Command::new("git")
                .args(["status", "--short"])
                .current_dir(workspace)
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();

            let mut diff_cmd = Command::new("git");
            diff_cmd.arg("diff");
            if !file_filter.is_empty() {
                diff_cmd.arg("--").arg(file_filter);
            }
            let diff_out = diff_cmd
                .current_dir(workspace)
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();

            let combined = format!(
                "[GIT STATUS]:\n{}\n\n[GIT DIFF]:\n{}",
                if status_out.trim().is_empty() { "(Limpio)" } else { status_out.trim() },
                if diff_out.trim().is_empty() { "(Sin diferencias en archivos rastreados)" } else { diff_out.trim() }
            );
            compact_log_output(&combined, max_shell_chars)
        }
        _ => format!("[ERROR]: Herramienta '{}' aún no implementada o no reconocida.", tool_name),
    }
}

/// Devuelve instrucciones breves del entorno OS y shell activo (Windows PowerShell vs Linux sh/bash)
pub fn runtime_os_shell_hint() -> &'static str {
    if cfg!(target_os = "windows") {
        "[Entorno OS: Windows | Shell: PowerShell] Reglas de terminal: NO uses `&&` ni `||` (PowerShell los rechaza); encadena comandos con `;`. Usa sintaxis PowerShell (`Get-ChildItem`, `New-Item -ItemType Directory -Force`, `where.exe`). Nota: `write_file` crea directorios padre automáticamente."
    } else {
        "[Entorno OS: Linux/Unix | Shell: sh/bash] Reglas de terminal: Puedes usar `&&`, `;`, `mkdir -p`, `ls`, `which`, `ss`. Nota: `write_file` crea directorios padre automáticamente."
    }
}

/// Smart Log Compactor: Si la salida supera `max_chars`, conserva la cabecera y el bloque final (donde vive el Traceback/Error)
pub fn compact_log_output(raw: &str, max_chars: usize) -> String {
    if raw.len() <= max_chars {
        return raw.to_string();
    }
    let head_len = max_chars / 3;
    let tail_len = max_chars - head_len;
    let head: String = raw.chars().take(head_len).collect();
    let total_chars = raw.chars().count();
    let tail: String = raw
        .chars()
        .skip(total_chars.saturating_sub(tail_len))
        .collect();

    format!(
        "{}\n\n... [SMART LOG COMPACTOR: {} caracteres intermedios omitidos para proteger la KV Cache] ...\n\n{}",
        head,
        total_chars.saturating_sub(max_chars),
        tail
    )
}

const MAX_PROJECT_MEMORY_CHARS: usize = 4000;

fn walk_workspace_files(
    workspace: &Path,
    subdir: &str,
    max_depth: usize,
) -> impl Iterator<Item = walkdir::DirEntry> {
    let target = resolve_path(workspace, subdir);
    WalkDir::new(target)
        .max_depth(max_depth)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".git"
                && name != "target"
                && name != "node_modules"
                && name != "__pycache__"
                && name != "dist"
                && name != "build"
        })
        .flatten()
        .filter(|e| e.file_type().is_file())
}

fn rel_display(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}

fn glob_matches(pattern: &str, rel_path: &str) -> bool {
    let pat = pattern.trim().replace('\\', "/");
    if pat.is_empty() || pat == "*" {
        return true;
    }
    let norm_path = rel_path.replace('\\', "/");
    let file_name = norm_path.rsplit('/').next().unwrap_or(&norm_path);
    let target = if pat.contains('/') {
        norm_path.as_str()
    } else {
        file_name
    };

    if !pat.contains('*') && !pat.contains('?') {
        return norm_path.to_lowercase().contains(&pat.to_lowercase());
    }

    let mut regex_str = String::from("(?i)^");
    for ch in pat.chars() {
        match ch {
            '*' => regex_str.push_str(".*"),
            '?' => regex_str.push('.'),
            other => regex_str.push_str(&regex::escape(&other.to_string())),
        }
    }
    regex_str.push('$');

    Regex::new(&regex_str)
        .map(|re| re.is_match(target))
        .unwrap_or(false)
}

fn handle_project_memory(workspace: &Path, args: &Value) -> String {
    let action = args["action"].as_str().unwrap_or("read").trim().to_lowercase();
    let mem_path = workspace.join(".harness").join("MEMORY.md");

    match action.as_str() {
        "read" => match fs::read_to_string(&mem_path) {
            Ok(raw) if !raw.trim().is_empty() => compact_log_output(raw.trim(), 2500),
            _ => "(Sin memoria persistente en .harness/MEMORY.md aún)".to_string(),
        },
        "append" => {
            let note = args["content"].as_str().unwrap_or("").trim();
            if note.is_empty() {
                return "[ERROR]: Falta 'content' para 'append'".to_string();
            }
            if let Some(parent) = mem_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let existing = fs::read_to_string(&mem_path).unwrap_or_default();
            let entry = if note.starts_with('-') || note.starts_with('#') {
                note.to_string()
            } else {
                format!("- {}", note)
            };
            let mut updated = if existing.trim().is_empty() {
                format!("# Memoria Persistente del Proyecto\n\n{}\n", entry)
            } else {
                format!("{}\n{}\n", existing.trim_end(), entry)
            };
            if updated.chars().count() > MAX_PROJECT_MEMORY_CHARS {
                // Conservar las entradas más recientes dentro del presupuesto KV
                let total = updated.chars().count();
                let tail: String = updated
                    .chars()
                    .skip(total.saturating_sub(MAX_PROJECT_MEMORY_CHARS - 80))
                    .collect();
                updated = format!("# Memoria Persistente del Proyecto\n...\n{}", tail);
            }
            match fs::write(&mem_path, &updated) {
                Ok(_) => format!("[OK] Nota agregada a {}", mem_path.display()),
                Err(e) => format!("[ERROR escribiendo memoria]: {}", e),
            }
        }
        "replace" => {
            let content = args["content"].as_str().unwrap_or("").trim();
            if content.is_empty() {
                return "[ERROR]: Falta 'content' para 'replace'".to_string();
            }
            if let Some(parent) = mem_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let bounded: String = content.chars().take(MAX_PROJECT_MEMORY_CHARS).collect();
            match fs::write(&mem_path, format!("{}\n", bounded)) {
                Ok(_) => format!("[OK] Memoria actualizada en {}", mem_path.display()),
                Err(e) => format!("[ERROR escribiendo memoria]: {}", e),
            }
        }
        _ => "[ERROR]: 'action' debe ser 'read', 'append' o 'replace'".to_string(),
    }
}

/// Busca la región más cercana al bloque `search` fallido dentro de `norm_orig`
/// y devuelve ~12-15 líneas numeradas alrededor de la mejor coincidencia.
fn find_closest_snippet(norm_orig: &str, norm_search: &str) -> Option<String> {
    let orig_lines: Vec<&str> = norm_orig.lines().collect();
    if orig_lines.is_empty() {
        return None;
    }

    let search_lines: Vec<&str> = norm_search
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if search_lines.is_empty() {
        return None;
    }

    let first = search_lines[0];

    // 1) Coincidencia exacta recortada de la primera línea no vacía de `search`
    let exact_first_matches: Vec<usize> = orig_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim() == first)
        .map(|(idx, _)| idx)
        .collect();

    let best_idx = if !exact_first_matches.is_empty() {
        exact_first_matches
            .into_iter()
            .max_by_key(|&idx| {
                let window_end = (idx + search_lines.len() + 2).min(orig_lines.len());
                let matched = search_lines
                    .iter()
                    .filter(|&&s| {
                        orig_lines[idx..window_end]
                            .iter()
                            .any(|ol| ol.trim() == s)
                    })
                    .count();
                // Desempate a favor de la primera aparición (índice menor)
                (matched, std::cmp::Reverse(idx))
            })
    } else {
        // 2) Fallback por mayor coincidencia de tokens (o subcadena) entre líneas de `search` y `orig`
        let tokenize = |s: &str| -> Vec<String> {
            s.split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|t| t.len() >= 2)
                .map(|t| t.to_lowercase())
                .collect()
        };

        let search_tokens: Vec<(&str, Vec<String>)> = search_lines
            .iter()
            .map(|&s| (s, tokenize(s)))
            .collect();

        let mut best: Option<(usize, usize)> = None;
        for (idx, &orig_line) in orig_lines.iter().enumerate() {
            let trimmed_orig = orig_line.trim();
            if trimmed_orig.is_empty() {
                continue;
            }
            let orig_tokens = tokenize(trimmed_orig);
            let mut line_best_score = 0usize;

            for (s_idx, (s_line, s_toks)) in search_tokens.iter().enumerate() {
                let score = if trimmed_orig == *s_line {
                    200 + s_line.len()
                } else if trimmed_orig.len() >= 4
                    && s_line.len() >= 4
                    && (trimmed_orig.contains(*s_line) || s_line.contains(trimmed_orig))
                {
                    100 + trimmed_orig.len().min(s_line.len())
                } else if !s_toks.is_empty() && !orig_tokens.is_empty() {
                    let overlap: usize = s_toks
                        .iter()
                        .filter(|t| orig_tokens.iter().any(|ot| ot == *t))
                        .count();
                    let overlap_chars: usize = s_toks
                        .iter()
                        .filter(|t| orig_tokens.iter().any(|ot| ot == *t))
                        .map(|t| t.len())
                        .sum();
                    if overlap > 0 {
                        overlap * 10 + overlap_chars + if s_idx == 0 { 5 } else { 0 }
                    } else {
                        0
                    }
                } else {
                    0
                };
                if score > line_best_score {
                    line_best_score = score;
                }
            }

            if line_best_score > 0 {
                let replace_best = match best {
                    None => true,
                    Some((_, prev_score)) => line_best_score > prev_score,
                };
                if replace_best {
                    best = Some((idx, line_best_score));
                }
            }
        }
        best.map(|(idx, _)| idx)
    };

    let idx = best_idx?;
    let window = 14usize;
    let mut start = idx.saturating_sub(5);
    let end = (start + window).min(orig_lines.len());
    if end == orig_lines.len() {
        start = end.saturating_sub(window);
    }

    let snippet = orig_lines[start..end]
        .iter()
        .enumerate()
        .map(|(offset, line)| format!("{:>4} | {}", start + offset + 1, line))
        .collect::<Vec<_>>()
        .join("\n");

    Some(snippet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_and_find_files_filter_cleanly() {
        assert!(glob_matches("*.rs", "src/core/client.rs"));
        assert!(!glob_matches("*.toml", "src/core/client.rs"));
        assert!(glob_matches("src/*.rs", "src/main.rs"));
        assert!(glob_matches("relay", "cowork_mobile/relay/Cargo.toml"));
    }

    #[tokio::test]
    async fn project_memory_read_append_replace_roundtrip() {
        let dir = std::env::temp_dir().join("lh_project_memory_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let r0 = execute_local_tool(
            "project_memory",
            r#"{"action":"read"}"#,
            &dir,
            4000,
            "http://127.0.0.1:8080/v1",
        )
        .await;
        assert!(r0.contains("Sin memoria"));

        let r1 = execute_local_tool(
            "project_memory",
            r#"{"action":"append","content":"Puerto API 8090"}"#,
            &dir,
            4000,
            "http://127.0.0.1:8080/v1",
        )
        .await;
        assert!(r1.contains("[OK]"));

        let r2 = execute_local_tool(
            "project_memory",
            r#"{"action":"read"}"#,
            &dir,
            4000,
            "http://127.0.0.1:8080/v1",
        )
        .await;
        assert!(r2.contains("Puerto API 8090"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_path_strips_redundant_workspace_prefix() {
        let base = std::env::temp_dir().join(format!(
            "oai_resolve_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let ws = base.join("cowork_mobile");
        let auth_dir = ws.join("api").join("src").join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        let jwt = auth_dir.join("jwt.rs");
        fs::write(&jwt, "// jwt").unwrap();

        assert_eq!(
            resolve_path(&ws, "cowork_mobile/api/src/auth/jwt.rs"),
            jwt
        );
        assert_eq!(
            resolve_path(&ws, "cowork_mobile\\api\\src\\auth\\jwt.rs"),
            jwt
        );
        assert_eq!(
            resolve_path(&ws, "cowork_mobile/api/src/auth/new_mod.rs"),
            auth_dir.join("new_mod.rs")
        );
        assert_eq!(
            resolve_path(&ws, "api/src/auth/jwt.rs"),
            jwt
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn apply_diff_error_includes_closest_snippet() {
        let base = std::env::temp_dir().join(format!(
            "oai_apply_diff_snippet_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        let sample = "\
// line 1
// line 2
// line 3
// line 4
// line 5
// line 6
// line 7
// line 8
fn calculate_score(items: &[i32]) -> i32 {
    let mut total = 0;
    for item in items {
        total += item;
    }
    total
}
// line 16
// line 17
// line 18
// line 19
// line 20
";
        fs::write(base.join("calc.rs"), sample).unwrap();

        // Caso 1: primera línea coincide recortada, pero falla la indentación/línea siguiente
        let args = json!({
            "path": "calc.rs",
            "search": "fn calculate_score(items: &[i32]) -> i32 {\n  let mut total = 999;",
            "replace": "fn calculate_score(items: &[i32]) -> i32 {\n    let mut total = 1;"
        });
        let err = execute_local_tool(
            "apply_diff",
            &args.to_string(),
            &base,
            4000,
            "http://127.0.0.1:8080/v1",
        )
        .await;

        assert!(err.starts_with("[ERROR apply_diff]"), "got: {}", err);
        assert!(err.contains("Fragmento más cercano"), "got: {}", err);
        assert!(err.contains("   9 | fn calculate_score(items: &[i32]) -> i32 {"), "got: {}", err);
        assert!(err.contains("  10 |     let mut total = 0;"), "got: {}", err);

        // Caso 2: fallback por coincidencia de tokens cuando la primera línea tiene una ligera diferencia
        let fallback = find_closest_snippet(
            sample,
            "pub fn calculate_score(items: &[u64]) -> u64 {\n    let total = 1;",
        )
        .expect("should find closest snippet by tokens");
        assert!(fallback.contains("9 | fn calculate_score(items: &[i32]) -> i32 {"));

        // Caso 3: búsqueda sin coincidencias retorna None
        assert!(find_closest_snippet(sample, "zzzz_nonexistent_token_xyz").is_none());

        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn multiple_apply_diff_calls_apply_independently_and_preserve_good_hunks() {
        let base = std::env::temp_dir().join(format!(
            "oai_multi_diff_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        let file = base.join("service.rs");
        fs::write(
            &file,
            "use old_crate::A;\n\nfn one() -> i32 {\n    1\n}\n\nfn two() -> i32 {\n    2\n}\n",
        )
        .unwrap();

        // Hunk 1: válido
        let r1 = execute_local_tool(
            "apply_diff",
            r#"{"path":"service.rs","search":"use old_crate::A;","replace":"use new_crate::A;"}"#,
            &base,
            4000,
            "http://127.0.0.1:8080/v1",
        )
        .await;
        assert!(r1.starts_with("[OK]"), "r1: {}", r1);

        // Hunk 2: inválido (no debe revertir el hunk 1 ni bloquear el hunk 3)
        let r2 = execute_local_tool(
            "apply_diff",
            r#"{"path":"service.rs","search":"fn one() -> i32 {\n    999\n}","replace":"fn one() -> i32 {\n    10\n}"}"#,
            &base,
            4000,
            "http://127.0.0.1:8080/v1",
        )
        .await;
        assert!(r2.starts_with("[ERROR apply_diff]"), "r2: {}", r2);
        assert!(r2.contains("Fragmento más cercano"), "r2: {}", r2);

        // Hunk 3: válido
        let r3 = execute_local_tool(
            "apply_diff",
            r#"{"path":"service.rs","search":"fn two() -> i32 {\n    2\n}","replace":"fn two() -> i32 {\n    20\n}"}"#,
            &base,
            4000,
            "http://127.0.0.1:8080/v1",
        )
        .await;
        assert!(r3.starts_with("[OK]"), "r3: {}", r3);

        let final_content = fs::read_to_string(&file).unwrap();
        assert!(final_content.contains("use new_crate::A;"));
        assert!(final_content.contains("fn one() -> i32 {\n    1\n}"));
        assert!(final_content.contains("fn two() -> i32 {\n    20\n}"));

        let _ = fs::remove_dir_all(&base);
    }
}

