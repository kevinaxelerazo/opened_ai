---
name: red-web-logic-ssrf
description: Evaluación de lógica de negocio, IDOR, Path Traversal, subida de archivos, XSS DOM y SSRF en aplicaciones web.
triggers:
  - web
  - api
  - idor
  - ssrf
  - xss
  - upload
  - traversal
  - endpoint
---

# 🕸️ Skill: Auditoría Avanzada Web App (IDOR, Lógica de Negocio, SSRF & DOM XSS)

## 1. Control de Acceso y Lógica de Negocio (IDOR / BOLA)
- En cada endpoint (`GET/POST/PUT/DELETE /api/resource/:id`), verificar que la consulta filtre tanto por `resource_id` como por el `user_id / tenant_id` autenticado en la sesión (nunca confiar en IDs enviados en el body o query).
- **Condiciones de Carrera (TOCTOU):** Revisar operaciones financieras, cupones, límites de uso o cambios de estado que hagan `SELECT` seguido de `UPDATE` sin transacción atómica o `SELECT ... FOR UPDATE`.

## 2. Manipulación de Archivos y Path Traversal
- Buscar concatenación de nombres de archivo controlados por el usuario en `fs::read`, `fs::write`, `sendFile` o `open()`.
- Exigir canonicalización de ruta (`canonicalize()`) y verificación estricta de que la ruta resultante comience con el directorio base permitido (`starts_with(base_dir)`).
- En subida de archivos (`multipart/form-data`), validar extensión contra lista blanca, verificar Magic Bytes reales, generar nombre UUID aleatorio y prohibir ejecución en la carpeta de destino.

## 3. SSRF y DOM XSS
- **SSRF:** Toda URL recibida del usuario antes de hacer `reqwest`/`fetch` debe parsearse, restringirse a `http/https`, resolver su DNS y bloquear rangos privados/loopback (`127.0.0.0/8`, `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `169.254.169.254`, `::1`).
- **DOM XSS:** En aplicaciones frontend, buscar asignaciones directas a `innerHTML`, `outerHTML`, `document.write`, `eval()` o `dangerouslySetInnerHTML` con datos de URL/storage/API sin sanitización (`textContent` o DOMPurify).

