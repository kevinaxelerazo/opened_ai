use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessConfig {
    pub server: ServerConfig,
    pub context_budget: ContextBudgetConfig,
    pub agents: HashMap<String, AgentConfig>,
    #[serde(skip)]
    pub loaded_from: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub base_url: String,
    pub slots_url: String,
    pub api_key: String,
    pub timeout_seconds: u64,
    pub enable_slot_caching: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBudgetConfig {
    pub max_context_tokens: usize,
    pub soft_limit_tokens: usize,
    pub strip_historical_thinking: bool,
    pub max_shell_output_chars: usize,
    pub auto_git_checkpoint: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub role: String,
    pub model: String,
    pub disposable_context: bool,
    pub temperature: f32,
    pub max_tokens: usize,
    pub max_iterations: usize,
    #[serde(default)]
    pub reasoning_budget: Option<usize>,
    #[serde(default)]
    pub auto_compact_context: Option<bool>,
    #[serde(default)]
    pub compact_threshold_tokens: Option<usize>,
    #[serde(default)]
    pub max_report_chars: Option<usize>,
    pub allowed_tools: Vec<String>,
    pub system_prompt: String,
}

impl HarnessConfig {
    /// Carga la configuración desde `--config`, `OPENED_AI_CONFIG`, workspace/proyecto,
    /// directorio de usuario (`~/.config/opened-ai/` o `%LOCALAPPDATA%\opened-ai\`)
    /// o plantilla del binario. Permite sobreescribir el servidor vía env vars o `--server`.
    #[allow(dead_code)]
    pub fn load(custom_path: Option<&Path>) -> Result<Self> {
        Self::load_for_workspace(custom_path, None)
    }

    pub fn load_for_workspace(custom_path: Option<&Path>, workspace: Option<&Path>) -> Result<Self> {
        if let Some(p) = custom_path {
            if !p.exists() {
                anyhow::bail!("No se encontró el archivo de configuración especificado: {:?}", p);
            }
            return Self::load_from_file(p);
        }

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let candidates = collect_config_candidates(workspace, &cwd);

        for path in &candidates {
            if path.exists() {
                return Self::load_from_file(path);
            }
        }

        anyhow::bail!(
            "No se encontró config.yaml / config.yml ni config.json. Rutas buscadas: {:?}",
            candidates
        )
    }

    fn load_from_file(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("No se pudo leer {:?}", path))?;
        let mut cfg: HarnessConfig =
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                serde_json::from_str(&raw)
                    .with_context(|| format!("Error parseando JSON en {:?}", path))?
            } else {
                serde_yaml::from_str(&raw)
                    .with_context(|| format!("Error parseando YAML en {:?}", path))?
            };
        cfg.loaded_from = Some(fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
        normalize_server_urls(&mut cfg);
        apply_env_server_overrides(&mut cfg);
        Ok(cfg)
    }

    /// Sobreescribe `server.base_url` y `server.slots_url` a partir de un host, IP o URL (`--server`).
    pub fn apply_server_override(&mut self, raw: &str) {
        let root = normalize_server_root(raw);
        if root.is_empty() {
            return;
        }
        self.server.base_url = format!("{}/v1", root);
        self.server.slots_url = format!("{}/slots", root);
    }
}

fn collect_config_candidates(workspace: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    for var in ["OPENED_AI_CONFIG", "LLAMA_HARNESS_CONFIG"] {
        if let Ok(env_cfg) = std::env::var(var) {
            let trimmed = env_cfg.trim();
            if !trimmed.is_empty() {
                candidates.push(PathBuf::from(trimmed));
                break;
            }
        }
    }

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(ws) = workspace {
        roots.push(ws.to_path_buf());
    }
    if roots.first().map(|p| p.as_path()) != Some(cwd) {
        roots.push(cwd.to_path_buf());
    }

    // 1) Overrides explícitos de proyecto/workspace (tienen máxima prioridad local)
    for root in &roots {
        for rel in [
            ".harness/config.local.yml",
            ".harness/config.local.yaml",
            ".harness/config.local.json",
            ".harness/config.yml",
            ".harness/config.yaml",
            ".harness/config.json",
            "config/config.local.yml",
            "config/config.local.yaml",
            "config/config.local.json",
            "config/config.yml",
            "opened-ai.yml",
            "opened-ai.yaml",
            "opened-ai.json",
            "config.yml",
            "config.yaml",
            "config.json",
        ] {
            candidates.push(root.join(rel));
        }
    }

    // 2) Configuración global de usuario (~/.config/opened-ai/ en Linux, %LOCALAPPDATA% en Windows)
    // Va ANTES que el fallback `config/config.yaml` del repo copiado para no volver a `127.0.0.1`.
    for dir in user_config_dirs() {
        for file in [
            "config.local.yml",
            "config.local.yaml",
            "config.local.json",
            "config.yml",
            "config.yaml",
            "config.json",
        ] {
            candidates.push(dir.join(file));
        }
    }

    // 3) Plantilla/fallback del repo local o junto al binario
    for root in &roots {
        candidates.push(root.join("config/config.yaml"));
        candidates.push(root.join("config/config.json"));
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    candidates.push(exe_dir.join("config/config.yml"));
    candidates.push(exe_dir.join("config/config.yaml"));
    candidates.push(exe_dir.join("config.yml"));
    candidates.push(exe_dir.join("config.yaml"));

    #[cfg(windows)]
    {
        candidates.push(PathBuf::from(r"b:\develop\opened-ai\config\config.yaml"));
        candidates.push(PathBuf::from(r"b:\develop\llama_harness\config\config.yaml"));
    }

    candidates
}

fn user_config_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for app_dir in ["opened-ai", "llama-harness"] {
        #[cfg(windows)]
        {
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                if !local.is_empty() {
                    dirs.push(PathBuf::from(local).join(app_dir));
                }
            }
            if let Ok(roaming) = std::env::var("APPDATA") {
                if !roaming.is_empty() {
                    dirs.push(PathBuf::from(roaming).join(app_dir));
                }
            }
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                dirs.push(PathBuf::from(xdg).join(app_dir));
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                dirs.push(PathBuf::from(home).join(".config").join(app_dir));
            }
        }
    }
    dirs
}

fn first_non_empty_env(names: &[&str]) -> Option<String> {
    for n in names {
        if let Ok(val) = std::env::var(n) {
            let trimmed = val.trim().trim_end_matches('/');
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn normalize_server_root(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/v1").unwrap_or(trimmed).trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.contains(':') {
        format!("http://{}", trimmed)
    } else {
        format!("http://{}:8080", trimmed)
    }
}

fn is_loopback_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("://127.0.0.1") || lower.contains("://localhost") || lower.contains("://[::1]")
}

fn normalize_server_urls(cfg: &mut HarnessConfig) {
    let root = normalize_server_root(&cfg.server.base_url);
    if !root.is_empty() {
        cfg.server.base_url = format!("{}/v1", root);
        let slots_trimmed = cfg.server.slots_url.trim().trim_end_matches('/');
        if slots_trimmed.is_empty() || (is_loopback_url(slots_trimmed) && !is_loopback_url(&root)) {
            cfg.server.slots_url = format!("{}/slots", root);
        }
    }
}

fn apply_env_server_overrides(cfg: &mut HarnessConfig) {
    if let Some(clean) = first_non_empty_env(&["OPENED_AI_BASE_URL", "LLAMA_HARNESS_BASE_URL"]) {
        cfg.apply_server_override(&clean);
    } else if let Some(clean) = first_non_empty_env(&["OPENED_AI_HOST", "LLAMA_HARNESS_HOST"]) {
        cfg.apply_server_override(&clean);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_config() -> HarnessConfig {
        HarnessConfig {
            server: ServerConfig {
                base_url: "http://127.0.0.1:8080/v1".to_string(),
                slots_url: "http://127.0.0.1:8080/slots".to_string(),
                api_key: "homelab".to_string(),
                timeout_seconds: 600,
                enable_slot_caching: true,
            },
            context_budget: ContextBudgetConfig {
                max_context_tokens: 32768,
                soft_limit_tokens: 20480,
                strip_historical_thinking: true,
                max_shell_output_chars: 4000,
                auto_git_checkpoint: true,
            },
            agents: HashMap::new(),
            loaded_from: None,
        }
    }

    #[test]
    fn env_server_overrides_normalize_host_and_base_url() {
        for var in [
            "OPENED_AI_BASE_URL",
            "OPENED_AI_HOST",
            "LLAMA_HARNESS_BASE_URL",
            "LLAMA_HARNESS_HOST",
        ] {
            std::env::remove_var(var);
        }

        std::env::set_var("OPENED_AI_HOST", "192.168.18.10");
        let mut cfg = sample_config();
        apply_env_server_overrides(&mut cfg);
        assert_eq!(cfg.server.base_url, "http://192.168.18.10:8080/v1");
        assert_eq!(cfg.server.slots_url, "http://192.168.18.10:8080/slots");

        std::env::set_var("OPENED_AI_HOST", "http://100.118.52.119:9000/");
        let mut cfg2 = sample_config();
        apply_env_server_overrides(&mut cfg2);
        assert_eq!(cfg2.server.base_url, "http://100.118.52.119:9000/v1");
        assert_eq!(cfg2.server.slots_url, "http://100.118.52.119:9000/slots");

        std::env::set_var("OPENED_AI_BASE_URL", "https://ia.ejemplo.net/v1/");
        let mut cfg3 = sample_config();
        apply_env_server_overrides(&mut cfg3);
        assert_eq!(cfg3.server.base_url, "https://ia.ejemplo.net/v1");
        assert_eq!(cfg3.server.slots_url, "https://ia.ejemplo.net/slots");

        std::env::remove_var("OPENED_AI_BASE_URL");
        std::env::remove_var("OPENED_AI_HOST");
        std::env::set_var("LLAMA_HARNESS_HOST", "192.168.18.20");
        let mut cfg4 = sample_config();
        apply_env_server_overrides(&mut cfg4);
        assert_eq!(cfg4.server.base_url, "http://192.168.18.20:8080/v1");

        std::env::remove_var("LLAMA_HARNESS_HOST");
    }

    #[test]
    fn normalize_server_urls_updates_loopback_slots_and_prefers_workspace_config_yml() {
        let mut cfg = sample_config();
        cfg.server.base_url = "http://192.168.18.10:8080".to_string();
        normalize_server_urls(&mut cfg);
        assert_eq!(cfg.server.base_url, "http://192.168.18.10:8080/v1");
        assert_eq!(cfg.server.slots_url, "http://192.168.18.10:8080/slots");

        let ws = PathBuf::from("/tmp/client_proj");
        let cwd = PathBuf::from("/tmp/harness_to_linux");
        let cands = collect_config_candidates(Some(&ws), &cwd);
        let pos_ws_root_yml = cands.iter().position(|p| p == &ws.join("config.yml")).unwrap();
        let pos_ws_cfg_yml = cands.iter().position(|p| p == &ws.join("config/config.yml")).unwrap();
        let pos_cwd_template = cands.iter().position(|p| p == &cwd.join("config/config.yaml")).unwrap();
        assert!(pos_ws_cfg_yml < pos_cwd_template);
        assert!(pos_ws_root_yml < pos_cwd_template);
    }
}

