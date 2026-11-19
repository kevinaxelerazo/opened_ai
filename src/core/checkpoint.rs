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
        .args(["diff", "--stat", "--", "."])
        .current_dir(workspace)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let status = Command::new("git")
        .args(["status", "--short", "--", "."])
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn git_diff_summary_scopes_to_workspace_subdirectory() {
        let base = std::env::temp_dir().join(format!(
            "oai_checkpoint_scope_test_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&base);
        let sub = base.join("cowork_mobile");
        fs::create_dir_all(&sub).unwrap();

        let run_git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&base)
                .output()
                .expect("git command should execute");
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };

        run_git(&["init"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test User"]);

        fs::write(base.join("parent_file.txt"), "initial parent\n").unwrap();
        fs::write(sub.join("child_file.txt"), "initial child\n").unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-m", "initial commit"]);

        // Cambios fuera del subdirectorio workspace no deben aparecer
        fs::write(base.join("parent_file.txt"), "modified parent\n").unwrap();
        fs::write(base.join("parent_untracked.txt"), "untracked parent\n").unwrap();

        let summary_clean = get_git_diff_summary(&sub);
        assert_eq!(summary_clean, "Sin cambios en archivos rastreados por Git.");
        assert!(!summary_clean.contains("parent_file.txt"));
        assert!(!summary_clean.contains("parent_untracked.txt"));

        // Cambios dentro del subdirectorio sí deben aparecer, sin filtrar los del padre
        fs::write(sub.join("child_file.txt"), "modified child\n").unwrap();
        fs::write(sub.join("child_untracked.txt"), "untracked child\n").unwrap();

        let summary_scoped = get_git_diff_summary(&sub);
        assert!(summary_scoped.contains("child_file.txt"));
        assert!(summary_scoped.contains("child_untracked.txt"));
        assert!(!summary_scoped.contains("parent_file.txt"));
        assert!(!summary_scoped.contains("parent_untracked.txt"));

        let _ = fs::remove_dir_all(&base);
    }
}

