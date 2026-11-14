use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(skip)]
    pub file_path: PathBuf,
}

/// Descubre todos los archivos `SKILL.md` en el directorio del harness y en el workspace actual
/// leyendo únicamente el frontmatter YAML (carga perezosa: 0 gasto de KV Cache hasta que se invoque).
pub fn discover_skills(workspace: &Path) -> Vec<SkillMetadata> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let mut roots = vec![
        workspace.join(".harness").join("skills"),
        workspace.join("skills"),
        exe_dir.join("skills"),
    ];
    for app_dir in ["opened-ai", "llama-harness"] {
        #[cfg(windows)]
        {
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                if !local.is_empty() {
                    roots.push(PathBuf::from(local).join(app_dir).join("skills"));
                }
            }
            if let Ok(roaming) = std::env::var("APPDATA") {
                if !roaming.is_empty() {
                    roots.push(PathBuf::from(roaming).join(app_dir).join("skills"));
                }
            }
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                roots.push(PathBuf::from(xdg).join(app_dir).join("skills"));
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                roots.push(PathBuf::from(home).join(".config").join(app_dir).join("skills"));
            }
        }
    }
    #[cfg(windows)]
    {
        roots.push(PathBuf::from(r"b:\develop\opened-ai\skills"));
        roots.push(PathBuf::from(r"b:\develop\llama_harness\skills"));
    }

    let mut seen_names = HashSet::new();
    let mut skills = Vec::new();

    for root in roots {
        if !root.exists() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let skill_md = path.join("SKILL.md");
                    if skill_md.exists() {
                        if let Some(meta) = parse_skill_frontmatter(&skill_md) {
                            if seen_names.insert(meta.name.clone()) {
                                skills.push(meta);
                            }
                        }
                    }
                }
            }
        }
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn parse_skill_frontmatter(skill_md: &Path) -> Option<SkillMetadata> {
    let raw = fs::read_to_string(skill_md).ok()?;
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        let folder_name = skill_md
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        return Some(SkillMetadata {
            name: folder_name,
            description: "Skill personalizada (sin frontmatter YAML)".to_string(),
            triggers: Vec::new(),
            file_path: skill_md.to_path_buf(),
        });
    }

    let after_first = &trimmed[3..];
    let end_idx = after_first.find("\n---")?;
    let yaml_block = &after_first[..end_idx];

    let mut meta: SkillMetadata = serde_yaml::from_str(yaml_block).ok()?;
    meta.file_path = skill_md.to_path_buf();
    Some(meta)
}

/// Devuelve el catálogo compacto de 1 línea por Skill (~60 tokens en total)
pub fn format_skills_catalog(workspace: &Path) -> String {
    let skills = discover_skills(workspace);
    if skills.is_empty() {
        return "No se encontraron skills instaladas en `skills/*/SKILL.md`.".to_string();
    }

    let mut out = String::from("📚 SKILLS DISPONIBLES (Carga Perezosa con `load_skill`):\n");
    for s in skills {
        let trig = if s.triggers.is_empty() {
            String::new()
        } else {
            format!(" [Triggers: {}]", s.triggers.join(", "))
        };
        out.push_str(&format!("- `{}`: {}{}\n", s.name, s.description, trig));
    }
    out
}

/// Carga el contenido completo de un `SKILL.md` bajo demanda en el contexto efímero del agente
pub fn load_skill_body(workspace: &Path, skill_name: &str) -> String {
    let skills = discover_skills(workspace);
    let target = skill_name.trim().to_lowercase();

    for s in &skills {
        if s.name.to_lowercase() == target || s.name.to_lowercase().contains(&target) {
            return match fs::read_to_string(&s.file_path) {
                Ok(content) => format!(
                    "=== 📚 SKILL CARGADA: `{}` ({}) ===\n{}",
                    s.name,
                    s.file_path.display(),
                    content
                ),
                Err(e) => format!("[ERROR] No se pudo leer {:?}: {}", s.file_path, e),
            };
        }
    }

    let available: Vec<String> = skills.into_iter().map(|s| s.name).collect();
    format!(
        "[ERROR] Skill `{}` no encontrada. Skills disponibles: {:?}",
        skill_name, available
    )
}

/// Evalúa automáticamente si el texto de la tarea (`task_brief`) coincide con los `triggers`
/// de alguna `SKILL.md` instalada, inyectando hasta `max_skills` playbooks directamente en el
/// contexto desechable del subagente (como hacen Claude Code / Superpowers / Claude-Red).
pub fn match_triggered_skills(
    workspace: &Path,
    task_text: &str,
    max_skills: usize,
) -> Option<(Vec<String>, String)> {
    let skills = discover_skills(workspace);
    let lower_task = task_text.to_lowercase();
    let mut matched_names = Vec::new();
    let mut combined_body = String::new();

    for s in skills {
        let hit = s.triggers.iter().any(|t| {
            let tl = t.trim().to_lowercase();
            if tl.is_empty() {
                return false;
            }
            // Coincidencia por límites de palabra para evitar falsos positivos de subcadenas
            // (ej. "ui" en "construir", "port" en "soporte", "env" en "enviar")
            let pat = format!(r"(?i)(?:^|[^a-z0-9_]){}(?:$|[^a-z0-9_])", regex::escape(&tl));
            Regex::new(&pat)
                .map(|re| re.is_match(&lower_task))
                .unwrap_or_else(|_| lower_task.contains(&tl))
        });
        if hit {
            if let Ok(raw) = fs::read_to_string(&s.file_path) {
                matched_names.push(s.name.clone());
                combined_body.push_str(&format!(
                    "\n--- [AUTO-SKILL ACTIVADA POR TRIGGER: `{}`] ---\n{}\n",
                    s.name,
                    raw.trim()
                ));
                if matched_names.len() >= max_skills {
                    break;
                }
            }
        }
    }

    if matched_names.is_empty() {
        None
    } else {
        Some((matched_names, combined_body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_skills_loads_mermaid_and_hermes_skills() {
        let ws = Path::new(env!("CARGO_MANIFEST_DIR"));
        let skills = discover_skills(ws);
        let names: Vec<String> = skills.into_iter().map(|s| s.name).collect();
        for expected in [
            "mermaid-diagrams",
            "sdlc-code-review",
            "dogfood-qa",
            "blocked-page-recovery",
            "rest-api-debug",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "Falta skill esperada: {} (encontradas: {:?})",
                expected,
                names
            );
        }

        let matched = match_triggered_skills(ws, "Crea un diagrama mermaid de la API REST", 2);
        let (matched_names, _) = matched.expect("Debería activar trigger mermaid / rest");
        assert!(matched_names.contains(&"mermaid-diagrams".to_string()));
    }
}


