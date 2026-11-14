---
name: rest-api-debug
description: Protocolo sistemático de verificación para endpoints HTTP REST, autenticación JWT, CORS, payloads JSON y streams SSE.
triggers:
  - rest
  - api
  - jwt
  - sse
  - endpoint
  - cors
  - webhook
  - relay
---

# 🔌 Skill: Depuración Sistemática de APIs REST, JWT y SSE

Guía rápida para `qa_tester` y `semi_sr_dev` al validar servidores HTTP locales.

## Checklist de Verificación Real
1. **Arranque y Salud:** lanza el backend con `bg_start`, revisa `bg_logs` y comprueba el endpoint de salud (`GET /health` o `/v1/models`) verificando código HTTP `200`.
2. **Auth & Contratos Negativos:**
   - Petición sin `Authorization: Bearer ...` en ruta protegida → debe devolver `401` (nunca `200` ni `500` con stacktrace).
   - Payload JSON inválido o campos faltantes → debe devolver `400`/`422` claro.
3. **Streaming SSE (`text/event-stream`):**
   - Verifica cabeceras `Content-Type: text/event-stream` y eventos `data: ...\n\n` en vivo (con `curl.exe -N` en PowerShell o `curl -N` en bash) sin buffering bloqueante.
4. **Cierre limpio:** al finalizar detén cualquier servidor de pruebas con `bg_kill` y reporta comandos exactos + códigos HTTP observados.

