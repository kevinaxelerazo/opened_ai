---
name: owasp-deep-audit
description: Metodología avanzada de auditoría de código (Taint Analysis Source-to-Sink) para OWASP Top 10 y CWE Top 25.
triggers:
  - security
  - owasp
  - audit
  - vulnerabilidad
  - cve
---

# 🛡️ Skill: Metodología de Auditoría Profunda OWASP Top 10 (Source-to-Sink)

## 1. Trazabilidad de Flujo de Datos (Taint Analysis)
1. **Identificar Sources (Fuentes no confiables):**
   - Parámetros HTTP (`query`, `body`, `headers`, `cookies`), payloads WebSocket, variables CLI, archivos subidos o respuestas de APIs externas.
2. **Rastrear Propagación:**
   - Seguir la variable a través de funciones y módulos con `search_code` y `read_file`. Verificar si las funciones validadoras usan listas blancas estrictas o regex vulnerables a bypass.
3. **Auditar Sinks (Sumideros críticos):**
   - **Ejecución de Comandos:** `Command::new`, `sh -c`, `powershell -Command`, `os.system`, `subprocess.Popen(shell=True)`, `child_process.exec`.
   - **Sistema de Archivos:** `fs::read`, `fs::write`, `open()`, `sendFile` sin canonicalización ni verificación de prefijo (riesgo de Path Traversal `../`).
   - **Consultas SQL / NoSQL:** Concatenación de strings o `format!()` en lugar de consultas parametrizadas (`?`, `$1`).
   - **Peticiones Salientes (SSRF):** `reqwest::get(user_url)`, `fetch(user_url)` sin bloqueo de rangos RFC1918 (`127.0.0.0/8`, `10.0.0.0/8`, `192.168.0.0/16`, `169.254.169.254`).

## 2. Plantilla de Hallazgo Técnico
Cada vulnerabilidad confirmada debe incluir:
- **[SEVERIDAD — CATEGORÍA OWASP / CWE]** Título técnico.
- **Ubicación:** `ruta/archivo.ext:línea`.
- **Flujo Source $\rightarrow$ Sink:** Explicación exacta de cómo el dato sin sanitizar llega al sink.
- **Remediación Defensiva:** Bloque de código corregido listo para aplicar con `apply_diff`.

