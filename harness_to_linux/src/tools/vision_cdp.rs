use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use regex::Regex;
use reqwest::Client;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Herramienta `vision_inspect`:
/// 1. Si `target` es una URL (`http://...`) o un archivo `.html` local, renderiza la página en
///    Chrome/Edge Headless en el viewport solicitado (`mobile` 375x812, `tablet` 768x1024, `desktop` 1440x900)
///    guardando la captura PNG real en `.harness/screenshots/` y extrayendo métricas DOM/CSS de responsiveness.
/// 2. Si `target` es una imagen local (`.png`, `.jpg`, `.jpeg`, `.webp`), la carga directamente.
/// 3. Codifica la captura en Base64 y consulta al modelo multimodal local (`qwen36-uncensored` con su `mmproj`)
///    a través de `llama-swap` (`http://127.0.0.1:8080/v1/chat/completions`).
pub async fn tool_vision_inspect(args: &Value, workspace: &Path, server_base_url: &str) -> String {
    let raw_target = args
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if raw_target.is_empty() {
        return "[ERROR vision_inspect]: Falta el parámetro obligatorio `target` (URL, ruta a archivo .html o ruta a imagen .png/.jpg).".to_string();
    }

    let viewport = args
        .get("viewport")
        .and_then(|v| v.as_str())
        .unwrap_or("desktop")
        .to_lowercase();

    let custom_prompt = args
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("Analiza visualmente esta captura de pantalla o diagrama. Identifica problemas de diseño, alineación, textos cortados o superpuestos, bajo contraste, roturas de responsiveness o errores visuales, y propón mejoras concretas de CSS/HTML.");

    let vision_model = args
        .get("vision_model")
        .and_then(|v| v.as_str())
        .unwrap_or("qwen36-uncensored");

    let resolved_path = resolve_local_path(workspace, raw_target);
    let ext = resolved_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let is_direct_image = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp");

    let (image_path, dom_report) = if is_direct_image {
        if !resolved_path.exists() {
            return format!(
                "[ERROR vision_inspect]: No existe el archivo de imagen {:?}",
                resolved_path
            );
        }
        (
            resolved_path,
            "📷 Modo Imagen Directa (Diagrama / Captura existente en disco)".to_string(),
        )
    } else {
        // Es una URL o un archivo .html local -> Capturar con Chrome/Edge Headless
        let target_url = if raw_target.starts_with("http://")
            || raw_target.starts_with("https://")
            || raw_target.starts_with("file://")
        {
            raw_target.to_string()
        } else {
            if !resolved_path.exists() {
                return format!(
                    "[ERROR vision_inspect]: No se encontró el archivo HTML local {:?}",
                    resolved_path
                );
            }
            let abs = fs::canonicalize(&resolved_path).unwrap_or(resolved_path.clone());
            let clean_abs = abs
                .to_string_lossy()
                .replace("\\\\?\\", "")
                .replace('\\', "/");
            format!("file:///{}", clean_abs)
        };

        let (width, height, vp_label) = match viewport.as_str() {
            "mobile" => (375, 812, "mobile_375x812"),
            "tablet" => (768, 1024, "tablet_768x1024"),
            _ => (1440, 900, "desktop_1440x900"),
        };

        let screenshots_dir = workspace.join(".harness").join("screenshots");
        let _ = fs::create_dir_all(&screenshots_dir);
        let screenshot_file = screenshots_dir.join(format!("capture_{}.png", vp_label));

        match capture_headless_screenshot(&target_url, width, height, &screenshot_file) {
            Ok(_) => {
                let dom_metrics = inspect_dom_and_responsiveness(&target_url, &resolved_path);
                (screenshot_file, dom_metrics)
            }
            Err(e) => {
                return format!(
                    "[ERROR vision_inspect capturando pantalla con Chrome/Edge]: {}",
                    e
                );
            }
        }
    };

    // Leer imagen y codificar en Base64 para el proyector mmproj-F16
    let img_bytes = match fs::read(&image_path) {
        Ok(b) => b,
        Err(e) => {
            return format!(
                "[ERROR vision_inspect leyendo captura {:?}]: {}",
                image_path, e
            )
        }
    };

    let mime = match image_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/png",
    };

    let b64_data = BASE64.encode(&img_bytes);
    let data_uri = format!("data:{};base64,{}", mime, b64_data);

    // Consultar al modelo multimodal en llama-swap (:8080/v1/chat/completions)
    let vision_analysis = query_multimodal_llm(
        server_base_url,
        vision_model,
        custom_prompt,
        &dom_report,
        &data_uri,
    )
    .await;

    format!(
        "👁️ [REPORTE DE INSPECCIÓN MULTIMODAL (`vision_inspect`)]\n\
         • Captura procesada: `{}` ({:.1} KB)\n\
         • Modelo de Visión: `{}` (`mmproj-F16`)\n\n\
         📊 [TELEMETRÍA ESTRUCTURAL DOM / CSS]:\n{}\n\n\
         🧠 [DICTAMEN VISUAL DEL MODELO MULTIMODAL]:\n{}",
        image_path.display(),
        (img_bytes.len() as f64) / 1024.0,
        vision_model,
        dom_report,
        vision_analysis
    )
}

fn find_browser_binary() -> Option<PathBuf> {
    let candidates = if cfg!(target_os = "windows") {
        vec![
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        ]
    } else {
        vec![
            "/usr/bin/google-chrome",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/microsoft-edge",
        ]
    };

    for c in candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn capture_headless_screenshot(
    url: &str,
    width: u32,
    height: u32,
    out_png: &Path,
) -> Result<(), String> {
    let browser = find_browser_binary().ok_or_else(|| {
        "No se encontró Google Chrome ni Microsoft Edge instalado para captura headless.".to_string()
    })?;

    if out_png.exists() {
        let _ = fs::remove_file(out_png);
    }

    let window_arg = format!("--window-size={},{}", width, height);
    let screenshot_arg = format!("--screenshot={}", out_png.display());

    let output = Command::new(&browser)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--no-sandbox",
            "--allow-file-access-from-files",
            "--virtual-time-budget=2500",
            &window_arg,
            &screenshot_arg,
            url,
        ])
        .output()
        .map_err(|e| format!("Error ejecutando {:?}: {}", browser, e))?;

    if !out_png.exists() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "El navegador terminó (código {:?}) pero no generó {:?}. Detalle: {}",
            output.status.code(),
            out_png,
            stderr.trim()
        ));
    }

    Ok(())
}

/// Analiza el DOM/HTML y CSS para detectar métricas objetivas de responsiveness y accesibilidad
fn inspect_dom_and_responsiveness(url: &str, local_path: &Path) -> String {
    let raw_html = if local_path.exists() {
        fs::read_to_string(local_path).unwrap_or_default()
    } else if let Some(browser) = find_browser_binary() {
        Command::new(browser)
            .args([
                "--headless=new",
                "--disable-gpu",
                "--no-sandbox",
                "--dump-dom",
                url,
            ])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };

    if raw_html.trim().is_empty() {
        return "- (No se pudo extraer DOM estático; análisis basado 100% en la captura PNG)".to_string();
    }

    let lower = raw_html.to_lowercase();
    let has_viewport_meta = lower.contains("name=\"viewport\"") || lower.contains("name='viewport'");
    let media_queries = lower.matches("@media").count();
    let flex_uses = lower.matches("display: flex").count() + lower.matches("display:flex").count();
    let grid_uses = lower.matches("display: grid").count() + lower.matches("display:grid").count();
    let img_count = lower.matches("<img").count();
    let section_count = lower.matches("<section").count();

    // Detectar anchos fijos peligrosos (ej. width: 1200px sin max-width)
    let fixed_width_hazards = Regex::new(r"(?i)\bwidth\s*:\s*([5-9]\d{2}|\d{4,})px")
        .ok()
        .map(|re| re.find_iter(&raw_html).count())
        .unwrap_or(0);

    format!(
        "- Etiqueta `<meta name=\"viewport\">`: {}\n\
         - Breakpoints `@media` detectados: {}\n\
         - Layouts modernos: {} bloques Flexbox | {} bloques CSS Grid\n\
         - Secciones `<section>`: {} | Imágenes `<img>`: {}\n\
         - Alertas de ancho fijo rígido (`width: >=500px`): {}",
        if has_viewport_meta { "✅ Presente" } else { "❌ AUSENTE (Riesgo crítico en Mobile)" },
        media_queries,
        flex_uses,
        grid_uses,
        section_count,
        img_count,
        if fixed_width_hazards == 0 {
            "0 (Diseño fluido OK)".to_string()
        } else {
            format!("⚠️ {} declaración(es) rígida(s) que podrían causar overflow en móvil", fixed_width_hazards)
        }
    )
}

async fn query_multimodal_llm(
    server_base_url: &str,
    model: &str,
    prompt: &str,
    dom_telemetry: &str,
    data_uri: &str,
) -> String {
    let client = match Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
    {
        Ok(c) => c,
        Err(e) => return format!("[ERROR cliente HTTP multimodal]: {}", e),
    };

    let endpoint = format!("{}/chat/completions", server_base_url.trim_end_matches('/'));
    let combined_question = format!(
        "Actúa como el Frontend Visual & Architecture Inspector de Opened-AI.\n\
         Telemetría DOM/CSS previa:\n{}\n\n\
         Instrucción de inspección sobre la imagen adjunta:\n{}",
        dom_telemetry, prompt
    );

    let payload = json!({
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": combined_question
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": data_uri
                        }
                    }
                ]
            }
        ],
        "temperature": 0.2,
        "max_tokens": 1536
    });

    match client.post(&endpoint).json(&payload).send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return format!(
                    "⚠️ El servidor multimodal devolvió HTTP {}: {}",
                    status, body
                );
            }
            if let Ok(parsed) = serde_json::from_str::<Value>(&body) {
                if let Some(content) = parsed["choices"][0]["message"]["content"].as_str() {
                    return content.trim().to_string();
                }
            }
            format!("Respuesta cruda del servidor multimodal: {}", body)
        }
        Err(e) => format!(
            "⚠️ Captura PNG y análisis DOM completados exitosamente, pero `llama-swap` no está activo en `{}` para ejecutar la inferencia con `{}` ({}).",
            endpoint, model, e
        ),
    }
}

fn resolve_local_path(workspace: &Path, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        workspace.join(p)
    }
}

