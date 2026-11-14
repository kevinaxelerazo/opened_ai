# 📱 Cowork Mobile — Blueprint de Arquitectura

## 1. Visión General

Sistema de cowork/control remoto desde móvil (iOS/Android) para continuar sesiones de `llama-harness` y monitorear el homelab Windows + Tailscale.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  MOBILE CLIENT (PWA / iOS App / Android App)                               │
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │  Web UI (Vue 3 + Pinia + Vuetify) — PWA con Service Worker            ││
│  │  • Chat en tiempo real (SSE)                                            ││
│  │  • Terminal interactiva (WebTTY / xterm.js)                             ││
│  │  • Lista de sesiones + checkpoints                                      ││
│  │  • Control de agentes (start/stop/resume)                               ││
│  └─────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────┘
                              │ HTTPS (Tailscale mTLS)
                              │ JWT Bearer Token
                              ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  RELAY LAYER (ttyd + MCP SSE Relay + REST API)                             │
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │  ttyd (WebTTY) — puerto 7681                                            ││
│  │  • Autenticación: X-Auth-Token header                                   ││
│  │  • Proxies stdin/stdout → ttyd → WebSocket → browser                   ││
│  │  • Limitado a `lh.exe` (no shell directo)                               ││
│  └─────────────────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │  MCP SSE Relay (Rust binary o script)                                   ││
│  │  • Consume MCP stdio de llama-harness                                   ││
│  │  • Expone endpoints SSE: /sse, /tools, /sessions                       ││
│  │  • Persiste checkpoints en .harness/                                    ││
│  └─────────────────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │  REST API (Rust actix-web o Python FastAPI)                             ││
│  │  • POST /api/sessions (crear)                                           ││
│  │  • GET  /api/sessions/:id (estado)                                      ││
│  │  • POST /api/sessions/:id/resume (reanudar)                            ││
│  │  • DELETE /api/sessions/:id (kill)                                      ││
│  │  • GET  /api/checkpoints/:id (descargar)                                ││
│  │  • JWT: Bearer token (exp 24h, refresh)                                 ││
│  └─────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────┘
                              │ stdio (pipe)
                              │ MCP JSON-RPC 2.0
                              ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  LLAMA-HARNESS (Rust, src/interfaces/mcp_server.rs, acp_server.rs)         │
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │  MCP Server (stdio) — ya existente, se extiende con SSE relay          ││
│  │  • tools/list, tools/call, initialize                                   ││
│  │  • Subagentes: ti_manager, semi_sr_dev, frontend_inspector, etc.       ││
│  └─────────────────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │  ACP Server (stdio) — ya existente, se extiende con SSE relay          ││
│  │  • session/new, session/prompt, agent/prompt                           ││
│  │  • Soporta @agent_name task syntax                                      ││
│  └─────────────────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │  Core: orchestrator.rs, checkpoint.rs, client.rs                       ││
│  │  • Checkpoints: .harness/checkpoints/{session_id}.json                 ││
│  │  • Session IDs: UUID v4 generados por relay                            ││
│  └─────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  TAILSCALE NETWORK (mTLS entre móvil ↔ homelab)                            │
│  • Tailscale Funnel (HTTPS) para acceso público opcional                   │
│  • Tailscale MagicDNS: llama-harness.tail-xxxx.ts.net                       ││
│  • mTLS: certificado auto-firmado + validación en relay                    ││
│  • Firewall Windows: abrir solo 7681 (ttyd) + puerto relay (ej. 8080)      ││
└─────────────────────────────────────────────────────────────────────────────┘
```

## 2. Capas Detalladas

### Capa 1 — Mobile Client (PWA)

| Componente | Tecnología | Responsable |
|---|---|---|
| UI Framework | Vue 3 + Vuetify 3 | `cowork_mobile/web/` |
| State | Pinia | `cowork_mobile/web/src/stores/` |
| Terminal | xterm.js + WebTTY | `cowork_mobile/web/src/components/Terminal.vue` |
| SSE Client | EventSource / fetch stream | `cowork_mobile/web/src/services/sse.ts` |
| Auth | JWT (localStorage + refresh) | `cowork_mobile/web/src/services/auth.ts` |
| PWA | Vite PWA plugin | `cowork_mobile/web/vite.config.ts` |

**Flujo de autenticación:**
```
Mobile → POST /api/auth/login {username, password} → JWT (access 2h + refresh 7d)
Mobile → POST /api/auth/refresh {refresh_token} → nuevo access token
Mobile → WebSocket / SSE con X-Auth-Token header
```

### Capa 2 — Relay Layer

| Componente | Puertos | Protocolo |
|---|---|---|
| ttyd (WebTTY) | `:7681` | WebSocket → stdin/stdout de `lh.exe` |
| MCP SSE Relay | `:8080` | HTTP SSE + JSON-RPC 2.0 |
| REST API | `:8081` | HTTPS + JWT Bearer |

**ttyd configuration (`/etc/ttyd/config` o flags):**
```bash
ttyd -p 7681 -t 10 -H "X-Auth-Token: <token>" \
  -- "lh.exe" "mcp"
```

**MCP SSE Relay (Rust o Python):**
```
stdin (pipe) → [MCP stdio server] → [SSE relay buffer] → /sse (EventSource)
stdout (pipe) ← [SSE relay buffer] ← /tools/call (POST)
```

**REST API endpoints:**
```
POST   /api/auth/login          → {access_token, refresh_token}
POST   /api/auth/refresh        → {access_token}
POST   /api/sessions            → {session_id, status}
GET    /api/sessions            → [{session_id, status, created_at}]
GET    /api/sessions/:id        → {session_id, status, checkpoint}
POST   /api/sessions/:id/resume → {status: "resumed"}
DELETE /api/sessions/:id        → {status: "killed"}
GET    /api/checkpoints/:id     → {checkpoint_data}
GET    /api/health              → {ttyd: ok, mcp: ok, harness: ok}
```

### Capa 3 — llama-harness (Extensiones)

**Extensiones necesarias en `mcp_server.rs`:**
- Nuevo modo `--sse` que expone `/sse` endpoint en lugar de stdio
- Persistencia de checkpoints en `.harness/checkpoints/{session_id}.json`
- Soporte para `session/new` y `session/resume` en el protocolo MCP

**Extensiones necesarias en `acp_server.rs`:**
- Nuevo modo `--sse` con endpoints `/sessions/new`, `/sessions/prompt`
- Persistencia de estado de sesión entre reinicios
- Soporte para `session/kill`

## 3. Seguridad

| Vector | Mitigación |
|---|---|
| Autenticación | JWT con `alg: RS256`, exp 2h, refresh 7d |
| Transporte | Tailscale mTLS (certs auto-firmados) |
| ttyd | `X-Auth-Token` header obligatorio, no expone shell |
| SSE | Token en URL o header, no en localStorage sin HttpOnly |
| OAuth2 | `state` criptográfico, `redirect_uri` whitelist exacta |
| Cookies | `HttpOnly`, `Secure`, `SameSite=Strict` |
| Firewall | Solo puertos 7681 + 8080 + 8081 abiertos |

## 4. Persistencia

```
.harness/
├── checkpoints/
│   ├── {session_id}.json          # Estado completo de sesión
│   └── {session_id}.meta.json     # Metadatos (agent, timestamp, status)
├── sessions/
│   └── active.json                # Lista de sesiones activas
└── auth/
    └── jwt_keys/                  # Claves RSA para JWT (RS256)
```

## 5. Diagrama de Flujo de Datos

```
[Móvil] ──HTTPS──▶ [ttyd:7681] ──pipe──▶ [lh.exe mcp] ──stdio──▶ [orchestrator]
                    │                              │
                    │                              ├─▶ [subagent: semi_sr_dev]
                    │                              ├─▶ [subagent: frontend_inspector]
                    │                              └─▶ [checkpoint.save()]
                    │
                    └──▶ [MCP SSE Relay:8080] ──SSE──▶ [Móvil: EventSource]
                                                       │
                                                       └──▶ [REST API:8081] ──JWT──▶ [Móvil: Pinia]
```
