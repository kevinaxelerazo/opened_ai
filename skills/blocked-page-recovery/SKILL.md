---
name: blocked-page-recovery
description: Estrategias limpias para web_researcher cuando web_rag encuentra errores 403, muros JS o bloqueos anti-bot sin caer en bucles.
triggers:
  - 403
  - 429
  - blocked
  - cloudflare
  - web_rag
  - documentacion
---

# 🌐 Skill: Recuperación de Documentación Bloqueada (`web_rag`)

Cuando una URL devuelve HTTP 403, contenido vacío por SPA JavaScript o bloqueo anti-bot, **no reintentes la misma URL en bucle ni intentes evadir CAPTCHAs**.

## Estrategia de Alternativas Limpias
1. **Busca fuentes amigables en texto/Markdown:**
   - Agrega términos como `github`, `raw markdown`, `README`, `docs.rs` o `llms.txt` en `web_rag(query=...)`.
   - Para crates de Rust usa `docs.rs/<crate>/latest/<crate>/` o el repositorio oficial en GitHub.
2. **Consulta documentación en árbol fuente:**
   - Si conoces el repo oficial, busca el archivo Markdown directo en `raw.githubusercontent.com/<org>/<repo>/main/README.md` o `docs/...`.
3. **Máximo 2 intentos por fuente:** si tras 2 consultas alternativas la web sigue bloqueada, reporta honestamente al `TI Manager` qué fuente estuvo inaccesible y entrega únicamente los datos verificados de las fuentes que sí respondieron.

