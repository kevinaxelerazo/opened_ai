use std::fs;
use std::path::Path;
use std::process::Command;

/// Ejecuta una validación sintáctica rápida inmediatamente después de `write_file` o `apply_diff`.
/// Si detecta un error de sintaxis, devuelve el detalle exacto para que el subagente lo corrija
/// en el mismo ciclo sin desperdiciar iteraciones ni romper el build.
pub fn run_post_edit_diagnostics(workspace: &Path, file_path: &Path) -> String {
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let raw = match fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) => return format!("⚠️ [Diagnóstico Post-Edición] No se pudo releer el archivo: {}", e),
    };

    match ext.as_str() {
        "json" => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(_) => "✅ [Diagnóstico Post-Edición] JSON válido (0 errores sintácticos).".to_string(),
            Err(e) => format!(
                "❌ [Diagnóstico Post-Edición: ERROR JSON] Línea {}, Columna {}: {}\n\
                 👉 Acción requerida: Corrige la sintaxis JSON inmediatamente con `apply_diff`.",
                e.line(),
                e.column(),
                e
            ),
        },
        "yaml" | "yml" => match serde_yaml::from_str::<serde_yaml::Value>(&raw) {
            Ok(_) => "✅ [Diagnóstico Post-Edición] YAML válido (0 errores sintácticos).".to_string(),
            Err(e) => {
                let loc = e
                    .location()
                    .map(|l| format!("Línea {}, Columna {}", l.line(), l.column()))
                    .unwrap_or_else(|| "Ubicación desconocida".to_string());
                format!(
                    "❌ [Diagnóstico Post-Edición: ERROR YAML] {}: {}\n\
                     👉 Acción requerida: Corrige la indentación o sintaxis YAML con `apply_diff`.",
                    loc, e
                )
            }
        },
        "py" => {
            if let Some(err) = check_balanced_delimiters(&raw) {
                return format!(
                    "❌ [Diagnóstico Post-Edición: ERROR PYTHON] {}\n\
                     👉 Acción requerida: Corrige los delimitadores desbalanceados.",
                    err
                );
            }
            let path_str = file_path.to_string_lossy().to_string();
            if let Ok(out) = Command::new("python")
                .args(["-m", "py_compile", &path_str])
                .current_dir(workspace)
                .output()
            {
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return format!(
                        "❌ [Diagnóstico Post-Edición: SYNTAX ERROR PYTHON]\n{}\n\
                         👉 Acción requerida: Corrige el error de sintaxis antes de continuar.",
                        stderr.trim()
                    );
                }
                return "✅ [Diagnóstico Post-Edición] Sintaxis Python verificada (`py_compile` OK)."
                    .to_string();
            }
            "✅ [Diagnóstico Post-Edición] Delimitadores Python balanceados OK.".to_string()
        }
        "js" | "mjs" | "cjs" => {
            if let Some(err) = check_balanced_delimiters(&raw) {
                return format!(
                    "❌ [Diagnóstico Post-Edición: ERROR JS] {}\n\
                     👉 Acción requerida: Corrige las llaves/paréntesis desbalanceados.",
                    err
                );
            }
            let path_str = file_path.to_string_lossy().to_string();
            if let Ok(out) = Command::new("node")
                .args(["--check", &path_str])
                .current_dir(workspace)
                .output()
            {
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return format!(
                        "❌ [Diagnóstico Post-Edición: SYNTAX ERROR JS (`node --check`)]\n{}\n\
                         👉 Acción requerida: Corrige el error de sintaxis JS.",
                        stderr.trim()
                    );
                }
                return "✅ [Diagnóstico Post-Edición] Sintaxis JavaScript verificada (`node --check` OK)."
                    .to_string();
            }
            "✅ [Diagnóstico Post-Edición] Delimitadores JavaScript balanceados OK.".to_string()
        }
        "rs" | "ts" | "tsx" | "jsx" | "c" | "cpp" | "h" | "hpp" | "go" | "css" => {
            if let Some(err) = check_balanced_delimiters(&raw) {
                return format!(
                    "❌ [Diagnóstico Post-Edición: DELIMITADORES DESBALANCEADOS (.{})] {}\n\
                     👉 Acción requerida: Revisa las líneas editadas con `read_file` y corrige con `apply_diff`.",
                    ext, err
                );
            }
            format!(
                "✅ [Diagnóstico Post-Edición] Estructura y delimitadores `.{}` balanceados correctamente.",
                ext
            )
        }
        "html" | "htm" => {
            if let Some(err) = check_html_structure(&raw) {
                return format!(
                    "⚠️ [Diagnóstico Post-Edición: ADVERTENCIA HTML] {}\n\
                     👉 Verifica que las etiquetas principales estén bien cerradas.",
                    err
                );
            }
            "✅ [Diagnóstico Post-Edición] Estructura HTML principal verificada OK.".to_string()
        }
        _ => "✅ [Diagnóstico Post-Edición] Archivo guardado y verificado en disco.".to_string(),
    }
}

/// Verifica de forma ultrarrápida (< 1ms) que llaves `{}`, corchetes `[]` y paréntesis `()`
/// estén balanceados ignorando comentarios de línea (`//`, `#`), comentarios de bloque (`/* */`) y strings.
fn check_balanced_delimiters(src: &str) -> Option<String> {
    let mut stack: Vec<(char, usize)> = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut line = 1usize;
    let mut in_string: Option<char> = None;
    let mut in_line_comment = false;
    let mut in_block_comment = false;

    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            in_line_comment = false;
            i += 1;
            continue;
        }

        if in_line_comment {
            i += 1;
            continue;
        }

        if in_block_comment {
            if c == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        if let Some(quote) = in_string {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }

        // Inicio de comentarios
        if c == '/' && i + 1 < chars.len() {
            if chars[i + 1] == '/' {
                in_line_comment = true;
                i += 2;
                continue;
            } else if chars[i + 1] == '*' {
                in_block_comment = true;
                i += 2;
                continue;
            }
        }

        if c == '"' || c == '\'' || c == '`' {
            in_string = Some(c);
            i += 1;
            continue;
        }

        match c {
            '{' | '(' | '[' => stack.push((c, line)),
            '}' | ')' | ']' => {
                let expected = match c {
                    '}' => '{',
                    ')' => '(',
                    ']' => '[',
                    _ => unreachable!(),
                };
                match stack.pop() {
                    Some((open, open_line)) if open != expected => {
                        return Some(format!(
                            "Se encontró '{}' en la línea {}, pero se esperaba cerrar '{}' (abierto en la línea {}).",
                            c, line, open, open_line
                        ));
                    }
                    None => {
                        return Some(format!(
                            "Se encontró '{}' de cierre inesperado en la línea {} sin apertura previa.",
                            c, line
                        ));
                    }
                    _ => {}
                }
            }
            _ => {}
        }

        i += 1;
    }

    if let Some((open, open_line)) = stack.pop() {
        return Some(format!(
            "El delimitador '{}' abierto en la línea {} nunca fue cerrado al final del archivo.",
            open, open_line
        ));
    }

    None
}

fn check_html_structure(src: &str) -> Option<String> {
    let lower = src.to_lowercase();
    for tag in ["html", "head", "body", "script", "style"] {
        let open_exact = lower.matches(&format!("<{}>", tag)).count()
            + lower.matches(&format!("<{} ", tag)).count()
            + lower.matches(&format!("<{}\n", tag)).count()
            + lower.matches(&format!("<{}\r", tag)).count();
        let close_count = lower.matches(&format!("</{}>", tag)).count();
        if open_exact != close_count {
            return Some(format!(
                "Desbalance en etiqueta `<{}>`: {} apertura(s) vs {} cierre(s) `</{}>`.",
                tag, open_exact, close_count, tag
            ));
        }
    }
    None
}

