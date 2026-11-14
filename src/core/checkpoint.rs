use anyhow::Result;
use std::path::Path;
use std::process::Command;

/// Crea un snapshot silencioso en Git (`git stash create`) sin alterar el working tree.
/// Si el directorio no es un repositorio Git o no tiene cambios, retorna `Ok(None)`.
pub fn create_git_checkpoint(workspace: &Path, label: &str) -> Result<Option<String>> {
    let out = Command::new("git")
        .args(["stash", "create", label])
        .current_dir(workspace)
        .output();

    match out {
        Ok(o) if o.status.success() => {
            let commit_hash = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if commit_hash.is_empty() {
                // Working tree limpio, devolvemos HEAD como punto de restauración
                let head = Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(workspace)
                    .output()?;
                if head.status.success() {
                    let h = String::from_utf8_lossy(&head.stdout).trim().to_string();
                    if !h.is_empty() {
                        return Ok(Some(h));
                    }
                }
                Ok(None)
            } else {
                // Guardamos una referencia en refs/harness/checkpoints/<timestamp>
                let _ = Command::new("git")
                    .args([
                        "update-ref",
                        &format!("refs/harness/last_checkpoint"),
                        &commit_hash,
                    ])
                    .current_dir(workspace)
                    .output();
                Ok(Some(commit_hash))
            }
        }
        _ => Ok(None),
    }
}

/// Obtiene un resumen compacto (`git status -s` + `git diff --stat`) para el reporte del subagente
pub fn get_git_diff_summary(workspace: &Path) -> String {
    let stat = Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(workspace)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let status = Command::new("git")
        .args(["status", "--short"])
        .current_dir(workspace)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    if stat.is_empty() && status.is_empty() {
        "Sin cambios en archivos rastreados por Git.".to_string()
    } else if stat.is_empty() {
        status
    } else {
        format!("{}\n{}", status, stat)
    }
}

