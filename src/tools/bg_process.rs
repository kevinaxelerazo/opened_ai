use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, File};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgProcessRecord {
    pub name: String,
    pub pid: u32,
    pub command: String,
    pub log_file: String,
}

fn registry_path(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".harness").join("bg_procs.json")
}

fn load_registry(workspace: &Path) -> Vec<BgProcessRecord> {
    let p = registry_path(workspace);
    if let Ok(raw) = fs::read_to_string(p) {
        serde_json::from_str(&raw).unwrap_or_default()
    } else {
        Vec::new()
    }
}

fn save_registry(workspace: &Path, list: &[BgProcessRecord]) {
    let p = registry_path(workspace);
    if let Some(parent) = p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_string_pretty(list) {
        let _ = fs::write(p, raw);
    }
}

/// Inicia un proceso en background (ej. servidor web, dev server, watcher) redirigiendo
/// stdout y stderr a `.harness/bg_logs/<name>.log` sin bloquear al agente.
pub async fn tool_bg_start(args: &Value, workspace: &Path) -> String {
    let cmd_str = args
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if cmd_str.is_empty() {
        return "[ERROR] `bg_start` requiere el parámetro `command`.".to_string();
    }

    let raw_name = args
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("bg_server");
    let safe_name: String = raw_name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();

    let logs_dir = workspace.join(".harness").join("bg_logs");
    if let Err(e) = fs::create_dir_all(&logs_dir) {
        return format!("[ERROR] No se pudo crear directorio de logs {:?}: {}", logs_dir, e);
    }

    let log_path = logs_dir.join(format!("{}.log", safe_name));
    let stdout_file = match File::create(&log_path) {
        Ok(f) => f,
        Err(e) => return format!("[ERROR] No se pudo crear archivo de log {:?}: {}", log_path, e),
    };
    let stderr_file = match stdout_file.try_clone() {
        Ok(f) => f,
        Err(e) => return format!("[ERROR] No se pudo clonar descriptor de log: {}", e),
    };

    let mut command = if cfg!(target_os = "windows") {
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-Command", cmd_str]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", cmd_str]);
        c
    };

    command
        .current_dir(workspace)
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));

    match command.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            // Esperar 800ms para comprobar si el proceso arrancó bien o falló al instante
            tokio::time::sleep(Duration::from_millis(800)).await;

            let initial_logs = fs::read_to_string(&log_path)
                .unwrap_or_default()
                .lines()
                .take(25)
                .collect::<Vec<_>>()
                .join("\n");

            if let Ok(Some(exit_status)) = child.try_wait() {
                return format!(
                    "⚠️ [bg_start] El proceso `{}` (PID {}) terminó prematuramente con estado {}.\n\
                     Logs iniciales:\n{}",
                    safe_name, pid, exit_status, initial_logs
                );
            }

            let mut reg = load_registry(workspace);
            reg.retain(|r| r.name != safe_name);
            reg.push(BgProcessRecord {
                name: safe_name.clone(),
                pid,
                command: cmd_str.to_string(),
                log_file: log_path.to_string_lossy().to_string(),
            });
            save_registry(workspace, &reg);

            format!(
                "✅ [bg_start] Proceso `{}` ejecutándose en segundo plano (PID: {}).\n\
                 📄 Archivo de log: `{}`\n\
                 📋 Salida inicial:\n{}",
                safe_name,
                pid,
                log_path.display(),
                if initial_logs.trim().is_empty() {
                    "(Sin salida en los primeros 800ms — proceso activo)"
                } else {
                    &initial_logs
                }
            )
        }
        Err(e) => format!("[ERROR] Falló el arranque en background de `{}`: {}", cmd_str, e),
    }
}

/// Lee las últimas líneas del log de un proceso en segundo plano y verifica su estado.
pub fn tool_bg_logs(args: &Value, workspace: &Path) -> String {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("bg_server");
    let tail_lines = args
        .get("tail_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(50) as usize;

    let reg = load_registry(workspace);
    let rec = reg.iter().find(|r| r.name == name);

    let log_path = match rec {
        Some(r) => std::path::PathBuf::from(&r.log_file),
        None => workspace
            .join(".harness")
            .join("bg_logs")
            .join(format!("{}.log", name)),
    };

    if !log_path.exists() {
        let active: Vec<String> = reg
            .iter()
            .map(|r| format!("{} (PID {})", r.name, r.pid))
            .collect();
        return format!(
            "[ERROR] No existe log para `{}`. Procesos registrados: {:?}",
            name, active
        );
    }

    let content = fs::read_to_string(&log_path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(tail_lines);
    let tail = lines[start..].join("\n");

    let pid_info = rec
        .map(|r| format!("PID registrado: {} (`{}`)", r.pid, r.command))
        .unwrap_or_else(|| "PID no registrado".to_string());

    format!(
        "[bg_logs: `{}` | {} | Mostrando últimas {} de {} líneas]\n{}",
        name,
        pid_info,
        lines.len() - start,
        lines.len(),
        if tail.trim().is_empty() {
            "(Log vacío)"
        } else {
            &tail
        }
    )
}

/// Detiene un proceso en segundo plano por `name` o `pid` (incluyendo su árbol de subprocesos).
pub fn tool_bg_kill(args: &Value, workspace: &Path) -> String {
    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let explicit_pid = args.get("pid").and_then(|v| v.as_u64()).map(|p| p as u32);

    let mut reg = load_registry(workspace);
    let target_pid = if let Some(pid) = explicit_pid {
        Some(pid)
    } else {
        reg.iter().find(|r| r.name == name).map(|r| r.pid)
    };

    let Some(pid) = target_pid else {
        return format!(
            "[ERROR] No se encontró proceso en background con name=`{}` ni pid explícito.",
            name
        );
    };

    let status_msg = if cfg!(target_os = "windows") {
        match Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()
        {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                format!("{} {}", stdout.trim(), stderr.trim())
            }
            Err(e) => format!("Error invocando taskkill: {}", e),
        }
    } else {
        match Command::new("kill").args(["-9", &pid.to_string()]).output() {
            Ok(_) => format!("Señal SIGKILL enviada al PID {}", pid),
            Err(e) => format!("Error enviando kill -9: {}", e),
        }
    };

    reg.retain(|r| r.pid != pid && r.name != name);
    save_registry(workspace, &reg);

    format!(
        "🛑 [bg_kill] Proceso `{}` (PID {}) finalizado.\nDetalle del sistema: {}",
        name,
        pid,
        status_msg.trim()
    )
}

