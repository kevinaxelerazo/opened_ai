use crate::core::client::{FunctionCall, ToolCall};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;

/// Parser tolerante para extraer llamadas a herramientas incluso si el modelo
/// emite `<tool_call>{...}</tool_call>` o bloques JSON en el texto en vez del campo `tool_calls`.
pub fn extract_tolerant_tool_calls(content: &str, existing: &[ToolCall]) -> Vec<ToolCall> {
    if !existing.is_empty() {
        return existing.to_vec();
    }

    let mut extracted = Vec::new();

    // Patrón 1: <tool_call>{"name": "...", "arguments": {...}}</tool_call>
    if let Ok(re) = Regex::new(r"(?s)<tool_call>\s*(\{.*?\})\s*</tool_call>") {
        for (idx, cap) in re.captures_iter(content).enumerate() {
            if let Some(tc) = parse_json_tool_block(&cap[1], idx) {
                extracted.push(tc);
            }
        }
    }
    if !extracted.is_empty() {
        return extracted;
    }

    // Patrón 2: Bloques ```tool_call o ```json que contengan {"tool"|"name": "...", "arguments"|"args": {...}}
    if let Ok(re) = Regex::new(r"(?s)```(?:tool_call|json)?\s*(\{.*?\})\s*```") {
        for (idx, cap) in re.captures_iter(content).enumerate() {
            if let Some(tc) = parse_json_tool_block(&cap[1], idx) {
                extracted.push(tc);
            }
        }
    }

    extracted
}

fn parse_json_tool_block(raw_json: &str, idx: usize) -> Option<ToolCall> {
    let val: Value = serde_json::from_str(raw_json.trim()).ok()?;
    let name = val
        .get("name")
        .or_else(|| val.get("tool"))
        .and_then(|v| v.as_str())?
        .to_string();

    let args_val = val
        .get("arguments")
        .or_else(|| val.get("args"))
        .or_else(|| val.get("parameters"))
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    let arguments = if let Some(s) = args_val.as_str() {
        s.to_string()
    } else {
        serde_json::to_string(&args_val).ok()?
    };

    Some(ToolCall {
        id: format!("call_tol_{}", idx),
        call_type: "function".to_string(),
        function: FunctionCall { name, arguments },
    })
}

/// Detector de bucles de alucinación (Loop & Entropy Guard)
/// Evita que un subagente repita la misma llamada con los mismos argumentos indefinidamente.
pub struct LoopGuard {
    call_counts: HashMap<String, usize>,
    max_repeats: usize,
}

impl LoopGuard {
    pub fn new(max_repeats: usize) -> Self {
        Self {
            call_counts: HashMap::new(),
            max_repeats,
        }
    }

    /// Registra una llamada y devuelve `Err(mensaje_correctivo)` si detecta un bucle
    pub fn check_call(&mut self, tool_name: &str, arguments: &str) -> Result<(), String> {
        let signature = format!("{}::{}", tool_name, arguments.trim());
        let count = self.call_counts.entry(signature).or_insert(0);
        *count += 1;

        if *count > self.max_repeats {
            Err(format!(
                "[LOOP GUARD BLOQUEADO]: Ya invocaste `{}` con exactamente los mismos argumentos {} veces seguidas. \
                 Cambia de enfoque, verifica los parámetros o concluye tu reporte.",
                tool_name, count
            ))
        } else {
            Ok(())
        }
    }
}

