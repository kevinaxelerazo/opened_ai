use crate::config::ServerConfig;
use crate::interfaces::tui_hud;
use anyhow::{Context, Result};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_result(call_id: impl Into<String>, tool_name: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(output.into()),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            name: Some(tool_name.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_tool_type")]
    pub call_type: String,
    pub function: FunctionCall,
}

fn default_tool_type() -> String {
    "function".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub tokens_per_sec: f64,
    pub elapsed_ms: u128,
}

pub struct LlamaClient {
    http: Client,
    cfg: ServerConfig,
    think_regex: Regex,
}

impl LlamaClient {
    pub fn new(cfg: ServerConfig) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_seconds))
            .build()?;
        let think_regex = Regex::new(r"(?s)<think>.*?</think>\s*")?;
        Ok(Self {
            http,
            cfg,
            think_regex,
        })
    }

    /// Elimina bloques <think>...</think> de mensajes pasados para ahorrar KV Cache
    pub fn strip_thinking(&self, text: &str) -> String {
        self.think_regex.replace_all(text, "").trim().to_string()
    }

    /// Prepara los mensajes podando cualquier bloque <think> histórico
    pub fn sanitize_history(&self, messages: &[ChatMessage], strip_thinking: bool) -> Vec<ChatMessage> {
        if !strip_thinking {
            return messages.to_vec();
        }
        messages
            .iter()
            .map(|m| {
                let mut clean = m.clone();
                if clean.role == "assistant" {
                    if let Some(ref c) = clean.content {
                        clean.content = Some(self.strip_thinking(c));
                    }
                }
                clean
            })
            .collect()
    }

    /// Extrae bloques <think>...</think> de `raw_content` en caso de que el servidor
    /// no los haya separado en `reasoning_content`.
    fn extract_inline_thinking(&self, raw: &str) -> String {
        let mut collected = Vec::new();
        for mat in self.think_regex.find_iter(raw) {
            let s = mat.as_str().trim();
            let inner = s
                .strip_prefix("<think>")
                .unwrap_or(s)
                .trim_end()
                .strip_suffix("</think>")
                .unwrap_or(s)
                .trim();
            if !inner.is_empty() {
                collected.push(inner.to_string());
            }
        }
        collected.join("\n")
    }

    /// Envía una petición de chat con Streaming SSE (`stream: true`) para actualizar el HUD en vivo,
    /// con fallback transparente a JSON estándar si el servidor responde sin SSE.
    pub async fn chat_completion(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools_schema: Option<Vec<Value>>,
        temperature: f32,
        max_tokens: usize,
        strip_thinking: bool,
    ) -> Result<LlmResponse> {
        let clean_messages = self.sanitize_history(messages, strip_thinking);
        let url = format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'));

        let mut payload = json!({
            "model": model,
            "messages": clean_messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "top_p": 0.9,
            "stream": true,
            "stream_options": { "include_usage": true },
            "timings_per_token": true,
        });

        if let Some(tools) = tools_schema {
            if !tools.is_empty() {
                payload["tools"] = Value::Array(tools);
            }
        }

        let t0 = Instant::now();
        let mut resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.cfg.api_key))
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("Error conectando a {}", url))?;

        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {} desde {}: {}", status, url, body_text);
        }

        // Fallback si el endpoint respondió JSON no-streaming
        if content_type.contains("application/json") && !content_type.contains("text/event-stream") {
            let body_text = resp.text().await?;
            return self.parse_non_streaming_response(&body_text, t0.elapsed().as_millis());
        }

        let mut raw_content = String::new();
        let mut reasoning = String::new();
        let mut tool_acc: BTreeMap<usize, (String, String, String, String)> = BTreeMap::new();
        let mut prompt_tokens: usize = 0;
        let mut completion_tokens: usize = 0;
        let mut tokens_per_sec: f64 = 0.0;
        let mut streamed_chunks: usize = 0;
        let mut line_buf: Vec<u8> = Vec::new();
        let mut last_ping = Instant::now();

        while let Some(chunk) = resp.chunk().await? {
            line_buf.extend_from_slice(&chunk);
            while let Some(pos) = line_buf.iter().position(|&b| b == b'\n') {
                let raw_line: Vec<u8> = line_buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&raw_line).trim().to_string();
                if line.is_empty() || !line.starts_with("data:") {
                    continue;
                }
                let data_str = line.trim_start_matches("data:").trim();
                if data_str == "[DONE]" {
                    continue;
                }
                let Ok(val) = serde_json::from_str::<Value>(data_str) else {
                    continue;
                };

                if let Some(usage) = val.get("usage") {
                    if let Some(pt) = usage["prompt_tokens"].as_u64() {
                        prompt_tokens = pt as usize;
                    }
                    if let Some(ct) = usage["completion_tokens"].as_u64() {
                        completion_tokens = ct as usize;
                    }
                }
                if let Some(tps) = val["timings"]["predicted_per_second"].as_f64() {
                    if tps > 0.0 {
                        tokens_per_sec = tps;
                    }
                }

                if let Some(delta) = val["choices"][0].get("delta") {
                    if let Some(rc) = delta["reasoning_content"].as_str() {
                        reasoning.push_str(rc);
                        streamed_chunks += 1;
                    }
                    if let Some(c) = delta["content"].as_str() {
                        raw_content.push_str(c);
                        streamed_chunks += 1;
                    }
                    if let Some(tc_arr) = delta["tool_calls"].as_array() {
                        for tc in tc_arr {
                            let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                            let entry = tool_acc.entry(idx).or_insert_with(|| {
                                (String::new(), "function".to_string(), String::new(), String::new())
                            });
                            if let Some(id) = tc["id"].as_str() {
                                if !id.is_empty() {
                                    entry.0 = id.to_string();
                                }
                            }
                            if let Some(tp) = tc["type"].as_str() {
                                if !tp.is_empty() {
                                    entry.1 = tp.to_string();
                                }
                            }
                            if let Some(fn_obj) = tc.get("function") {
                                if let Some(name) = fn_obj["name"].as_str() {
                                    entry.2.push_str(name);
                                }
                                if let Some(args) = fn_obj["arguments"].as_str() {
                                    entry.3.push_str(args);
                                    streamed_chunks += 1;
                                }
                            }
                        }
                    }
                }

                if last_ping.elapsed() >= Duration::from_millis(250) {
                    let secs = t0.elapsed().as_secs_f64().max(0.05);
                    let live_tps = if tokens_per_sec > 0.0 {
                        tokens_per_sec
                    } else {
                        streamed_chunks as f64 / secs
                    };
                    tui_hud::set_streaming_progress(streamed_chunks, live_tps);
                    last_ping = Instant::now();
                }
            }
        }

        // Procesar posible cola sin \n final
        if !line_buf.is_empty() {
            let line = String::from_utf8_lossy(&line_buf).trim().to_string();
            if let Some(data_str) = line.strip_prefix("data:").map(str::trim) {
                if data_str != "[DONE]" {
                    if let Ok(val) = serde_json::from_str::<Value>(data_str) {
                        if let Some(pt) = val["usage"]["prompt_tokens"].as_u64() {
                            prompt_tokens = pt as usize;
                        }
                        if let Some(ct) = val["usage"]["completion_tokens"].as_u64() {
                            completion_tokens = ct as usize;
                        }
                        if let Some(tps) = val["timings"]["predicted_per_second"].as_f64() {
                            if tps > 0.0 {
                                tokens_per_sec = tps;
                            }
                        }
                    }
                }
            }
        }

        let elapsed_ms = t0.elapsed().as_millis();
        if reasoning.trim().is_empty() {
            let inline = self.extract_inline_thinking(&raw_content);
            if !inline.is_empty() {
                reasoning = inline;
            }
        }
        let clean_content = self.strip_thinking(&raw_content);

        let tool_calls: Vec<ToolCall> = tool_acc
            .into_values()
            .filter(|(_, _, name, _)| !name.is_empty())
            .map(|(id, call_type, name, arguments)| ToolCall {
                id: if id.is_empty() {
                    format!("call_{}", elapsed_ms)
                } else {
                    id
                },
                call_type,
                function: FunctionCall { name, arguments },
            })
            .collect();

        if completion_tokens == 0 {
            completion_tokens = streamed_chunks;
        }
        if tokens_per_sec == 0.0 && elapsed_ms > 0 {
            tokens_per_sec = (completion_tokens as f64) / (elapsed_ms as f64 / 1000.0);
        }

        Ok(LlmResponse {
            content: clean_content,
            reasoning,
            tool_calls,
            prompt_tokens,
            completion_tokens,
            tokens_per_sec,
            elapsed_ms,
        })
    }

    fn parse_non_streaming_response(&self, body_text: &str, elapsed_ms: u128) -> Result<LlmResponse> {
        let data: Value = serde_json::from_str(body_text)
            .with_context(|| "Respuesta JSON inválida desde llama-server")?;

        let choice = &data["choices"][0];
        let msg = &choice["message"];

        let raw_content = msg["content"].as_str().unwrap_or("").to_string();
        let mut reasoning = msg["reasoning_content"].as_str().unwrap_or("").to_string();
        if reasoning.trim().is_empty() {
            reasoning = self.extract_inline_thinking(&raw_content);
        }
        let clean_content = self.strip_thinking(&raw_content);

        let mut tool_calls: Vec<ToolCall> = Vec::new();
        if let Some(tc_arr) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tc_arr {
                if let Ok(parsed) = serde_json::from_value::<ToolCall>(tc.clone()) {
                    tool_calls.push(parsed);
                }
            }
        }

        let prompt_tokens = data["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as usize;
        let completion_tokens = data["usage"]["completion_tokens"].as_u64().unwrap_or(0) as usize;
        let mut tokens_per_sec = data["timings"]["predicted_per_second"].as_f64().unwrap_or(0.0);
        if tokens_per_sec == 0.0 && elapsed_ms > 0 {
            tokens_per_sec = (completion_tokens as f64) / (elapsed_ms as f64 / 1000.0);
        }

        Ok(LlmResponse {
            content: clean_content,
            reasoning,
            tool_calls,
            prompt_tokens,
            completion_tokens,
            tokens_per_sec,
            elapsed_ms,
        })
    }

    /// Guarda el estado de la KV Cache del slot 0 (ej. antes de ejecutar un subagente)
    pub async fn save_slot(&self, slot_id: usize, filename: &str) -> Result<bool> {
        if !self.cfg.enable_slot_caching {
            return Ok(false);
        }
        let url = format!(
            "{}/{}?action=save",
            self.cfg.slots_url.trim_end_matches('/'),
            slot_id
        );
        let res = self
            .http
            .post(&url)
            .json(&json!({ "filename": filename }))
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        Ok(res.map(|r| r.status().is_success()).unwrap_or(false))
    }

    /// Restaura el estado de la KV Cache del slot 0 (ej. al volver al TI Manager)
    pub async fn restore_slot(&self, slot_id: usize, filename: &str) -> Result<bool> {
        if !self.cfg.enable_slot_caching {
            return Ok(false);
        }
        let url = format!(
            "{}/{}?action=restore",
            self.cfg.slots_url.trim_end_matches('/'),
            slot_id
        );
        let res = self
            .http
            .post(&url)
            .json(&json!({ "filename": filename }))
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        Ok(res.map(|r| r.status().is_success()).unwrap_or(false))
    }

    /// Verifica qué modelos están registrados en llama-swap
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/models", self.cfg.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await?;
        let val: Value = resp.json().await?;
        let mut models = Vec::new();
        if let Some(arr) = val["data"].as_array() {
            for item in arr {
                if let Some(id) = item["id"].as_str() {
                    models.push(id.to_string());
                }
            }
        }
        Ok(models)
    }

    fn server_root_url(&self) -> String {
        let trimmed = self.cfg.base_url.trim_end_matches('/');
        trimmed.strip_suffix("/v1").unwrap_or(trimmed).to_string()
    }

    /// Consulta `/running` en llama-swap y devuelve pares `(modelo, estado)`.
    pub async fn running_models(&self) -> Result<Vec<(String, String)>> {
        let url = format!("{}/running", self.server_root_url());
        let resp = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(Vec::new());
        }
        let val: Value = resp.json().await?;
        Ok(parse_running_models_json(&val))
    }

    /// Dispara el hot-swap en llama-swap sin tocar slots KV usando `/upstream/<modelo>/health`.
    /// Devuelve `Ok(true)` si tuvo que cargar/swappear el modelo, `Ok(false)` si ya estaba listo
    /// o si el servidor no expone proxy llama-swap (ej. llama-server directo).
    pub async fn ensure_model_loaded(&self, model: &str) -> Result<bool> {
        if let Ok(running) = self.running_models().await {
            if running
                .iter()
                .any(|(m, state)| m == model && state == "ready")
            {
                return Ok(false);
            }
        }

        let url = format!("{}/upstream/{}/health", self.server_root_url(), model);
        let resp = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(self.cfg.timeout_seconds))
            .send()
            .await
            .with_context(|| format!("Error precargando `{}` en {}", model, url))?;

        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(false);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {} precargando `{}`: {}", status, model, body);
        }
        Ok(true)
    }

    /// Consulta `/upstream/<modelo>/props` (y `/props` como fallback) para leer el `n_ctx` real del servidor.
    pub async fn model_context_size(&self, model: &str) -> Option<usize> {
        let root = self.server_root_url();
        for url in [
            format!("{}/upstream/{}/props", root, model),
            format!("{}/props", root),
        ] {
            let Ok(resp) = self
                .http
                .get(&url)
                .timeout(Duration::from_secs(3))
                .send()
                .await
            else {
                continue;
            };
            if !resp.status().is_success() {
                continue;
            }
            if let Ok(val) = resp.json::<Value>().await {
                if let Some(n_ctx) = parse_props_n_ctx(&val) {
                    return Some(n_ctx);
                }
            }
        }
        None
    }
}

fn parse_props_n_ctx(val: &Value) -> Option<usize> {
    val["n_ctx"]
        .as_u64()
        .or_else(|| val["default_generation_settings"]["n_ctx"].as_u64())
        .map(|n| n as usize)
        .filter(|&n| n > 0)
}

fn parse_running_models_json(val: &Value) -> Vec<(String, String)> {
    val["running"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let m = item["model"].as_str()?;
                    let st = item["state"].as_str().unwrap_or("unknown");
                    Some((m.to_string(), st.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_running_models_extracts_model_and_state() {
        let sample = json!({
            "running": [
                { "model": "qwen38-uncensored", "state": "ready" },
                { "model": "qwen38-distill", "state": "stopping" }
            ]
        });
        let parsed = parse_running_models_json(&sample);
        assert_eq!(
            parsed,
            vec![
                ("qwen38-uncensored".to_string(), "ready".to_string()),
                ("qwen38-distill".to_string(), "stopping".to_string()),
            ]
        );
    }

    #[test]
    fn server_root_url_strips_v1_suffix() {
        let cfg = ServerConfig {
            base_url: "http://127.0.0.1:8080/v1/".to_string(),
            slots_url: "http://127.0.0.1:8080/slots".to_string(),
            api_key: "local".to_string(),
            timeout_seconds: 600,
            enable_slot_caching: true,
        };
        let client = LlamaClient::new(cfg).expect("client");
        assert_eq!(client.server_root_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn parse_props_n_ctx_reads_root_and_default_settings() {
        let root_props = json!({ "n_ctx": 49152 });
        assert_eq!(parse_props_n_ctx(&root_props), Some(49152));

        let nested_props = json!({ "default_generation_settings": { "n_ctx": 32768 } });
        assert_eq!(parse_props_n_ctx(&nested_props), Some(32768));

        let empty = json!({});
        assert_eq!(parse_props_n_ctx(&empty), None);
    }
}

