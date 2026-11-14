use regex::Regex;
use reqwest::Client;
use serde_json::Value;
use std::env;
use std::time::Duration;

/// Ejecuta una búsqueda técnica en la web o extrae el contenido limpio en Markdown de una URL,
/// truncando estrictamente la salida para proteger la KV Cache del subagente `web_researcher`.
///
/// Estrategia anti-CAPTCHA multi-fuente:
/// 1. Si existe `SEARXNG_URL` (ej. instancia local del homelab) -> usa JSON de SearXNG sin CAPTCHA.
/// 2. Si existe `TAVILY_API_KEY` -> usa Tavily Search API.
/// 3. Intenta DuckDuckGo HTML detectando retos CAPTCHA/bot (`anomaly.js` / `captcha` / 0 resultados).
/// 4. Fallback automático 100% libre de CAPTCHA (sin API key): GitHub Search API + HackerNews Algolia + Wikipedia API.
pub async fn tool_web_rag(args: &Value) -> String {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let direct_url = args
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let max_chars = args
        .get("max_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(5000) as usize;

    if query.is_empty() && direct_url.is_empty() {
        return "[ERROR web_rag]: Debes proporcionar `query` (búsqueda web) o `url` (lectura directa de página).".to_string();
    }

    let client = match Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .build()
    {
        Ok(c) => c,
        Err(e) => return format!("[ERROR web_rag creando cliente HTTP]: {}", e),
    };

    // Caso 1: Lectura directa de una URL técnica
    if !direct_url.is_empty() {
        return fetch_and_clean_url(&client, direct_url, max_chars).await;
    }

    // Caso 2A: SearXNG auto-hospedado opcional (`SEARXNG_URL`)
    if let Ok(searx_url) = env::var("SEARXNG_URL") {
        let base = searx_url.trim().trim_end_matches('/');
        if !base.is_empty() {
            if let Some(out) = search_searxng(&client, base, query, max_chars).await {
                return out;
            }
        }
    }

    // Caso 2B: Tavily API opcional (`TAVILY_API_KEY`)
    if let Ok(tavily_key) = env::var("TAVILY_API_KEY") {
        if !tavily_key.trim().is_empty() {
            if let Some(out) = search_tavily(&client, tavily_key.trim(), query, max_chars).await {
                return out;
            }
        }
    }

    // Caso 2C: DuckDuckGo HTML con detección explícita de CAPTCHA
    let encoded_q = urlencoding_simple(query);
    let search_url = format!("https://html.duckduckgo.com/html/?q={}", encoded_q);

    let ddg_html = client
        .get(&search_url)
        .send()
        .await
        .ok()
        .and_then(|r| if r.status().is_success() { Some(r) } else { None });

    if let Some(resp) = ddg_html {
        if let Ok(html) = resp.text().await {
            if !is_captcha_challenge(&html) {
                let results = parse_ddg_html_results(&html, 5);
                if !results.is_empty() {
                    return format_search_results_with_extract(&client, "DuckDuckGo", query, &results, max_chars).await;
                }
            }
        }
    }

    // Caso 2D: Fallback automático Anti-CAPTCHA (APIs JSON oficiales: GitHub + HackerNews + Wikipedia)
    let fallback_results = search_open_tech_apis(&client, query).await;
    if !fallback_results.is_empty() {
        let mut out = String::from(
            "ℹ️ [DuckDuckGo solicitó CAPTCHA; usando fallback multi-fuente sin bloqueo: GitHub + HackerNews + Wikipedia]\n\n",
        );
        let body = format_search_results_with_extract(
            &client,
            "Multi-Fuente Técnica",
            query,
            &fallback_results,
            max_chars.saturating_sub(out.len()),
        )
        .await;
        out.push_str(&body);
        return out;
    }

    format!(
        "[WARN web_rag]: No se obtuvieron resultados para `{}` (DDG bloqueado por CAPTCHA y sin coincidencias en GitHub/HN/Wikipedia). Usa `web_rag` con una `url` directa (ej. docs oficiales o GitHub) sin reintentar la misma búsqueda.",
        query
    )
}

fn is_captcha_challenge(html: &str) -> bool {
    let lower = html.to_lowercase();
    lower.contains("anomaly.js")
        || lower.contains("bots use duckduckgo")
        || lower.contains("challenge-form")
        || lower.contains("select all squares")
        || lower.contains("g-recaptcha")
        || lower.contains("cf-turnstile")
}

async fn format_search_results_with_extract(
    client: &Client,
    engine: &str,
    query: &str,
    results: &[(String, String, String)],
    max_chars: usize,
) -> String {
    let mut out = format!(
        "🔍 [WEB RAG ({}) — Top {} Resultados para `{}`]\n\n",
        engine,
        results.len(),
        query
    );
    for (idx, (title, url, snippet)) in results.iter().enumerate() {
        out.push_str(&format!(
            "{}. **{}**\n   - URL: {}\n   - Resumen: {}\n\n",
            idx + 1,
            title,
            url,
            snippet
        ));
    }

    if let Some((_, first_url, _)) = results.first() {
        if first_url.starts_with("http") && out.len() + 600 < max_chars {
            let remaining = max_chars.saturating_sub(out.len());
            let deep_extract = fetch_and_clean_url(client, first_url, remaining).await;
            out.push_str(&format!(
                "---\n📄 [EXTRACTO AUTOMÁTICO DE LA FUENTE #1 ({})]:\n{}",
                first_url, deep_extract
            ));
        }
    }

    out
}

async fn search_searxng(
    client: &Client,
    base_url: &str,
    query: &str,
    max_chars: usize,
) -> Option<String> {
    let url = format!("{}/search?q={}&format=json", base_url, urlencoding_simple(query));
    let val: Value = client.get(&url).send().await.ok()?.json().await.ok()?;
    let mut results = Vec::new();
    if let Some(arr) = val["results"].as_array() {
        for item in arr.iter().take(5) {
            let title = item["title"].as_str().unwrap_or("").trim().to_string();
            let link = item["url"].as_str().unwrap_or("").trim().to_string();
            let content = item["content"].as_str().unwrap_or("").trim().to_string();
            if !title.is_empty() && !link.is_empty() {
                results.push((title, link, content));
            }
        }
    }
    if results.is_empty() {
        None
    } else {
        Some(format_search_results_with_extract(client, "SearXNG", query, &results, max_chars).await)
    }
}

async fn search_tavily(
    client: &Client,
    api_key: &str,
    query: &str,
    max_chars: usize,
) -> Option<String> {
    let payload = serde_json::json!({
        "api_key": api_key,
        "query": query,
        "max_results": 5
    });
    let val: Value = client
        .post("https://api.tavily.com/search")
        .json(&payload)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let mut results = Vec::new();
    if let Some(arr) = val["results"].as_array() {
        for item in arr.iter().take(5) {
            let title = item["title"].as_str().unwrap_or("").trim().to_string();
            let link = item["url"].as_str().unwrap_or("").trim().to_string();
            let content = item["content"].as_str().unwrap_or("").trim().to_string();
            if !title.is_empty() && !link.is_empty() {
                results.push((title, link, content));
            }
        }
    }
    if results.is_empty() {
        None
    } else {
        Some(format_search_results_with_extract(client, "Tavily", query, &results, max_chars).await)
    }
}

/// Consulta paralela a APIs técnicas públicas (GitHub + HackerNews + Wikipedia) sin CAPTCHAs
async fn search_open_tech_apis(client: &Client, query: &str) -> Vec<(String, String, String)> {
    let enc = urlencoding_simple(query);
    let gh_url = format!(
        "https://api.github.com/search/repositories?q={}&sort=stars&per_page=3",
        enc
    );
    let hn_url = format!(
        "https://hn.algolia.com/api/v1/search?query={}&tags=story&hitsPerPage=3",
        enc
    );
    let wp_url = format!(
        "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&format=json&srlimit=2",
        enc
    );

    let (gh_res, hn_res, wp_res) = tokio::join!(
        client
            .get(&gh_url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "opened-ai/0.1")
            .send(),
        client.get(&hn_url).send(),
        client
            .get(&wp_url)
            .header("User-Agent", "opened-ai/0.1")
            .send()
    );

    let mut out = Vec::new();

    if let Ok(resp) = gh_res {
        if let Ok(val) = resp.json::<Value>().await {
            if let Some(items) = val["items"].as_array() {
                for repo in items.iter().take(3) {
                    let name = repo["full_name"].as_str().unwrap_or("");
                    let url = repo["html_url"].as_str().unwrap_or("");
                    let desc = repo["description"].as_str().unwrap_or("Sin descripción");
                    let stars = repo["stargazers_count"].as_u64().unwrap_or(0);
                    if !name.is_empty() && !url.is_empty() {
                        out.push((
                            format!("[GitHub ★{}] {}", stars, name),
                            url.to_string(),
                            desc.to_string(),
                        ));
                    }
                }
            }
        }
    }

    if let Ok(resp) = hn_res {
        if let Ok(val) = resp.json::<Value>().await {
            if let Some(hits) = val["hits"].as_array() {
                for hit in hits.iter().take(3) {
                    let title = hit["title"].as_str().unwrap_or("");
                    let obj_id = hit["objectID"].as_str().unwrap_or("");
                    let url = hit["url"]
                        .as_str()
                        .filter(|u| !u.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("https://news.ycombinator.com/item?id={}", obj_id));
                    let pts = hit["points"].as_u64().unwrap_or(0);
                    if !title.is_empty() {
                        out.push((
                            format!("[HN {} pts] {}", pts, title),
                            url,
                            format!("Discusión técnica en Hacker News (id={})", obj_id),
                        ));
                    }
                }
            }
        }
    }

    if let Ok(resp) = wp_res {
        if let Ok(val) = resp.json::<Value>().await {
            if let Some(items) = val["query"]["search"].as_array() {
                for item in items.iter().take(2) {
                    let title = item["title"].as_str().unwrap_or("");
                    let snippet = strip_tags_and_entities(item["snippet"].as_str().unwrap_or(""));
                    if !title.is_empty() {
                        let slug = title.replace(' ', "_");
                        out.push((
                            format!("[Wikipedia] {}", title),
                            format!("https://en.wikipedia.org/wiki/{}", slug),
                            snippet,
                        ));
                    }
                }
            }
        }
    }

    out
}

async fn fetch_and_clean_url(client: &Client, url: &str, max_chars: usize) -> String {
    match client.get(url).send().await {
        Ok(resp) => {
            let status = resp.status();
            match resp.text().await {
                Ok(raw_html) => {
                    let cleaned = html_to_clean_markdown(&raw_html, max_chars);
                    format!(
                        "[Fuente: {} | HTTP {}]\n{}",
                        url,
                        status.as_u16(),
                        cleaned
                    )
                }
                Err(e) => format!("[ERROR leyendo cuerpo de {}]: {}", url, e),
            }
        }
        Err(e) => format!("[ERROR descargando {}]: {}", url, e),
    }
}

/// Extrae títulos, URLs reales y fragmentos de la versión HTML ligera de DuckDuckGo
fn parse_ddg_html_results(html: &str, limit: usize) -> Vec<(String, String, String)> {
    let mut results = Vec::new();
    let link_re = Regex::new(
        r#"(?is)<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#,
    )
    .ok();
    let snippet_re = Regex::new(
        r#"(?is)<a[^>]*class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</a>"#,
    )
    .ok();

    let Some(l_re) = link_re else {
        return results;
    };

    let snippets: Vec<String> = snippet_re
        .map(|s_re| {
            s_re.captures_iter(html)
                .filter_map(|c| c.get(1).map(|m| strip_tags_and_entities(m.as_str())))
                .collect()
        })
        .unwrap_or_default();

    for (idx, cap) in l_re.captures_iter(html).enumerate() {
        if results.len() >= limit {
            break;
        }
        let raw_href = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let real_url = extract_ddg_target_url(raw_href);
        let title = cap
            .get(2)
            .map(|m| strip_tags_and_entities(m.as_str()))
            .unwrap_or_default();
        let snippet = snippets.get(idx).cloned().unwrap_or_default();

        if !title.is_empty() && !real_url.is_empty() {
            results.push((title, real_url, snippet));
        }
    }

    results
}

fn extract_ddg_target_url(raw_href: &str) -> String {
    if let Some(pos) = raw_href.find("uddg=") {
        let after = &raw_href[pos + 5..];
        let encoded = after.split('&').next().unwrap_or(after);
        urldecode_simple(encoded)
    } else if raw_href.starts_with("//") {
        format!("https:{}", raw_href)
    } else {
        raw_href.to_string()
    }
}

/// Convierte HTML crudo a Markdown/Texto estructurado eliminando scripts, estilos, SVGs y anuncios
pub fn html_to_clean_markdown(html: &str, max_chars: usize) -> String {
    let mut work = html.to_string();

    // 1. Eliminar bloques ruidosos completos
    for tag in ["script", "style", "svg", "noscript", "nav", "footer", "iframe"] {
        if let Ok(re) = Regex::new(&format!(r"(?is)<{}\b[^>]*>.*?</{}>", tag, tag)) {
            work = re.replace_all(&work, " ").to_string();
        }
    }

    // 2. Preservar encabezados y bloques de código en formato Markdown
    let replacements = [
        (r"(?is)<h1\b[^>]*>(.*?)</h1>", "\n\n# $1\n"),
        (r"(?is)<h2\b[^>]*>(.*?)</h2>", "\n\n## $1\n"),
        (r"(?is)<h3\b[^>]*>(.*?)</h3>", "\n\n### $1\n"),
        (r"(?is)<pre\b[^>]*>(.*?)</pre>", "\n```\n$1\n```\n"),
        (r"(?is)<code\b[^>]*>(.*?)</code>", "`$1`"),
        (r"(?is)<li\b[^>]*>(.*?)</li>", "\n- $1"),
        (r"(?is)<p\b[^>]*>(.*?)</p>", "\n$1\n"),
        (r"(?is)<br\s*/?>", "\n"),
    ];

    for (pat, rep) in replacements {
        if let Ok(re) = Regex::new(pat) {
            work = re.replace_all(&work, rep).to_string();
        }
    }

    let clean = strip_tags_and_entities(&work);

    // 3. Compactar líneas vacías repetidas
    let mut lines: Vec<String> = Vec::new();
    let mut prev_empty = false;
    for line in clean.lines() {
        let t = line.trim();
        if t.is_empty() {
            if !prev_empty {
                lines.push(String::new());
                prev_empty = true;
            }
        } else {
            lines.push(t.to_string());
            prev_empty = false;
        }
    }

    let joined = lines.join("\n").trim().to_string();
    if joined.chars().count() <= max_chars {
        joined
    } else {
        let truncated: String = joined.chars().take(max_chars).collect();
        format!(
            "{}\n\n... [WEB RAG: Contenido truncado a {} caracteres para proteger la KV Cache]",
            truncated, max_chars
        )
    }
}

fn strip_tags_and_entities(input: &str) -> String {
    let without_tags = match Regex::new(r"(?is)<[^>]+>") {
        Ok(re) => re.replace_all(input, " ").to_string(),
        Err(_) => input.to_string(),
    };
    without_tags
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
}

fn urlencoding_simple(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn urldecode_simple(s: &str) -> String {
    let mut bytes = Vec::new();
    let raw = s.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            if let Ok(hex) = std::str::from_utf8(&raw[i + 1..i + 3]) {
                if let Ok(val) = u8::from_str_radix(hex, 16) {
                    bytes.push(val);
                    i += 3;
                    continue;
                }
            }
        } else if raw[i] == b'+' {
            bytes.push(b' ');
            i += 1;
            continue;
        }
        bytes.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).to_string()
}

