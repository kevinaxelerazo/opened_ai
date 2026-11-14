# 📋 Cowork Mobile — Plan de Implementación

## Resumen

| Fase | Nombre | Estimación | Estado |
|---|---|---|---|
| 1 | ttyd + MCP SSE Relay | 2-3 días | ⬜ |
| 2 | API Móvil REST (session list, resume, kill) | 2-3 días | ⬜ |
| 3 | Web UI Móvil PWA | 3-4 días | ⬜ |
| 4 | Integración ACP + Checkpoints | 2 días | ⬜ |
| **Total** | | **~2 semanas** | |

---

## Fase 1: ttyd + MCP SSE Relay

### Objetivo
Hacer que `lh.exe mcp` sea accesible vía ttyd (terminal web) y exponer el stream MCP como SSE para el cliente móvil.

### Archivos a crear/modificar

| Archivo | Acción | Descripción |
|---|---|---|
| `cowork_mobile/relay/mcp_sse_relay.rs` | **CREAR** | Relay Rust que consume stdio MCP y expone SSE |
| `cowork_mobile/relay/Cargo.toml` | **CREAR** | Dependencias: `tokio`, `sse`, `serde_json`, `anyhow` |
| `cowork_mobile/relay/README.md` | **CREAR** | Instrucciones de despliegue |
| `src/interfaces/mcp_server.rs` | **MODIFICAR** | Añadir modo `--sse` con `tokio::net::TcpListener` |
| `src/interfaces/acp_server.rs` | **MODIFICAR** | Añadir modo `--sse` con `tokio::net::TcpListener` |
| `src/main.rs` | **MODIFICAR** | Añadir subcommand `cowork` que lanza relay |

### Tareas detalladas

| # | Tarea | Estimación |
|---|---|---|
| 1.1 | Configurar ttyd con `lh.exe mcp` como proceso hijo | 0.5 días |
| 1.2 | Implementar `mcp_sse_relay.rs`: pipe stdin/stdout → SSE | 1 día |
| 1.3 | Implementar `--sse` en `mcp_server.rs`: `TcpListener` + `tokio::io::copy_bidirectional` | 1 día |
| 1.4 | Implementar `--sse` en `acp_server.rs` (mismo patrón) | 0.5 días |
| 1.5 | Configurar `src/main.rs` con subcommand `cowork` | 0.5 días |
| 1.6 | Prueba local: `lh.exe cowork` → ttyd + SSE en localhost | 0.5 días |

### Riesgos

| Riesgo | Probabilidad | Impacto | Mitigación |
|---|---|---|---|
| Windows + Tailscale NAT: ttyd no escucha en interfaz Tailscale | Alta | Alto | Configurar `ttyd -b 0.0.0.0` + regla firewall Windows |
| Pipe stdio se bloquea si cliente SSE se desconecta | Media | Medio | Buffer circular + reintentos automáticos |
| SSE no soportada en iOS Safari (EventSource) | Baja | Medio | Fallback a fetch polling cada 2s |

---

## Fase 2: API Móvil REST (session list, resume, kill)

### Objetivo
API REST que permite al cliente móvil listar sesiones, reanudarlas, matarlas y descargar checkpoints.

### Archivos a crear/modificar

| Archivo | Acción | Descripción |
|---|---|---|
| `cowork_mobile/api/server.rs` | **CREAR** | Servidor actix-web (o axum) con JWT auth |
| `cowork_mobile/api/routes/sessions.rs` | **CREAR** | CRUD de sesiones |
| `cowork_mobile/api/routes/auth.rs` | **CREAR** | Login/refresh JWT |
| `cowork_mobile/api/routes/health.rs` | **CREAR** | Health check |
| `cowork_mobile/api/middleware/jwt.rs` | **CREAR** | Middleware de autenticación |
| `cowork_mobile/api/config.rs` | **CREAR** | Configuración (puertos, JWT keys) |
| `src/core/checkpoint.rs` | **MODIFICAR** | Añadir `save_checkpoint()` y `load_checkpoint()` |
| `src/core/orchestrator.rs` | **MODIFICAR** | Añadir `run_orchestrator_with_session()` |
| `src/core/client.rs` | **MODIFICAR** | Añadir `LlamaClient::new_with_session()` |

### Tareas detalladas

| # | Tarea | Estimación |
|---|---|---|
| 2.1 | Definir esquema de datos: `Session`, `Checkpoint`, `AgentState` | 0.5 días |
| 2.2 | Implementar `save_checkpoint()` en `checkpoint.rs` → `.harness/checkpoints/` | 0.5 días |
| 2.3 | Implementar `load_checkpoint()` → restaura estado de sesión | 0.5 días |
| 2.4 | Implementar `run_orchestrator_with_session()` → reanuda desde checkpoint | 1 día |
| 2.5 | Implementar API REST: `/api/sessions` (CRUD) | 1 día |
| 2.6 | Implementar JWT auth: login con username/password, refresh token | 1 día |
| 2.7 | Integrar con relay: API → relay → `lh.exe` | 0.5 días |
| 2.8 | Prueba: curl → crear sesión → reanudar → matar | 0.5 días |

### Riesgos

| Riesgo | Probabilidad | Impacto | Mitigación |
|---|---|---|---|
| Windows: `std::process::Child` no se puede reanudar tras `kill()` | Alta | Alto | Usar `tokio::process::Child` + `kill()` + reinicio desde checkpoint |
| JWT con `alg: none` por error de configuración | Baja | Crítico | Validar `alg` en middleware, rechazar `none` explícitamente |
| Confusión de clave RS256 → HS256 (ataque JWT) | Baja | Crítico | Usar `jsonwebtoken` con `Algorithm::RS256` explícito |

---

## Fase 3: Web UI Móvil PWA

### Objetivo
Interfaz web responsive (PWA) que funcione en iOS Safari y Android Chrome.

### Archivos a crear/modificar

| Archivo | Acción | Descripción |
|---|---|---|
| `cowork_mobile/web/package.json` | **CREAR** | Vue 3 + Vite + Vuetify + PWA |
| `cowork_mobile/web/vite.config.ts` | **CREAR** | Config Vite + PWA plugin |
| `cowork_mobile/web/src/App.vue` | **CREAR** | Layout principal |
| `cowork_mobile/web/src/router/index.ts` | **CREAR** | Vue Router (login, sessions, chat, terminal) |
| `cowork_mobile/web/src/stores/session.ts` | **CREAR** | Pinia store de sesiones |
| `cowork_mobile/web/src/stores/auth.ts` | **CREAR** | Pinia store de auth |
| `cowork_mobile/web/src/services/api.ts` | **CREAR** | Client axios/fetch con interceptores JWT |
| `cowork_mobile/web/src/services/sse.ts` | **CREAR** | EventSource + fallback polling |
| `cowork_mobile/web/src/components/Chat.vue` | **CREAR** | Chat con mensajes del agente |
| `cowork_mobile/web/src/components/Terminal.vue` | **CREAR** | xterm.js + WebTTY |
| `cowork_mobile/web/src/components/SessionList.vue` | **CREAR** | Lista de sesiones con acciones |
| `cowork_mobile/web/src/components/AgentControl.vue` | **CREAR** | Botones start/stop/resume |
| `cowork_mobile/web/src/styles/main.css` | **CREAR** | CSS responsive (mobile-first) |
| `cowork_mobile/web/public/manifest.json` | **CREAR** | Web App Manifest |
| `cowork_mobile/web/public/sw.js` | **CREAR** | Service Worker (offline cache) |

### Tareas detalladas

| # | Tarea | Estimación |
|---|---|---|
| 3.1 | Scaffold Vue 3 + Vite + Vuetify + PWA plugin | 0.5 días |
| 3.2 | Layout mobile-first: bottom nav, chat area, terminal drawer | 1 día |
| 3.3 | Componente Chat: mensajes del agente + input de prompt | 1 día |
| 3.4 | Componente Terminal: xterm.js + WebSocket (ttyd) | 1 día |
| 3.5 | Componente SessionList: lista + acciones (resume, kill) | 0.5 días |
| 3.6 | Componente AgentControl: botones start/stop/resume | 0.5 días |
| 3.7 | SSE: EventSource + fallback polling + reconnection | 1 día |
| 3.8 | PWA: manifest, service worker, install prompt | 0.5 días |
| 3.9 | Testing multi-viewport: 375px, 768px, 1440px | 0.5 días |

### Riesgos

| Riesgo | Probabilidad | Impacto | Mitigación |
|---|---|---|---|
| iOS Safari: EventSource no soporta headers personalizados | Alta | Alto | Usar token en URL (`/sse?token=xxx`) en lugar de header |
| iOS Safari: Service Worker con HTTPS obligatorio | Media | Bajo | Documentar: requiere HTTPS (Tailscale Funnel o certificado) |
| iOS Safari: WebTTY no funciona en background | Media | Medio | Usar polling en lugar de WebSocket en background |
| Contraste WCAG AA en modo oscuro iOS | Baja | Bajo | Usar colores de Vuetify con contraste verificado |

---

## Fase 4: Integración ACP + Checkpoints

### Objetivo
Integrar el protocolo ACP (session/new, session/prompt, session/kill) con el relay y la UI.

### Archivos a crear/modificar

| Archivo | Acción | Descripción |
|---|---|---|
| `cowork_mobile/api/routes/acp.rs` | **CREAR** | Endpoints ACP: `/acp/session/new`, `/acp/session/prompt` |
| `cowork_mobile/api/routes/acp.rs` | **MODIFICAR** | Añadir `session/kill` |
| `src/core/checkpoint.rs` | **MODIFICAR** | Añadir `Checkpoint::to_json()` y `Checkpoint::from_json()` |
| `src/core/orchestrator.rs` | **MODIFICAR** | Añadir `run_orchestrator_from_checkpoint()` |
| `src/interfaces/acp_server.rs` | **MODIFICAR** | Añadir `session/kill` y `session/resume` |
| `src/interfaces/mcp_server.rs` | **MODIFICAR** | Añadir `session/kill` y `session/resume` |

### Tareas detalladas

| # | Tarea | Estimación |
|---|---|---|
| 4.1 | Implementar `session/kill` en ACP → `tokio::process::Child::kill()` | 0.5 días |
| 4.2 | Implementar `session/resume` → cargar checkpoint + reiniciar orchestrator | 1 día |
| 4.3 | Implementar `Checkpoint::to_json()` / `from_json()` serializable | 0.5 días |
| 4.4 | Integrar ACP endpoints en relay SSE | 0.5 días |
| 4.5 | Prueba: crear sesión → matar → reanudar desde checkpoint | 0.5 días |

### Riesgos

| Riesgo | Probabilidad | Impacto | Mitigación |
|---|---|---|---|
| Windows: `tokio::process::Child::kill()` no mata proceso hijo (ttyd) | Alta | Alto | Matar proceso padre (ttyd) + reiniciar con `--resume` flag |
| Checkpoint corrupto tras crash de Windows | Media | Medio | Validar checksum en `Checkpoint::from_json()` |
| Race condition: kill + resume simultáneo | Baja | Medio | Mutex en `orchestrator.rs` para lock de sesión |

---

## Cronograma Estimado

```
Semana 1:
  Día 1-2: Fase 1 (ttyd + MCP SSE Relay)
  Día 3-4: Fase 2 (API REST + JWT)
  Día 5:   Fase 2 (integración + pruebas)

Semana 2:
  Día 1-2: Fase 3 (Web UI + PWA)
  Día 3-4: Fase 3 (testing multi-viewport)
  Día 5:   Fase 4 (ACP + checkpoints)

Semana 3:
  Día 1-2: Integración final + pruebas end-to-end
  Día 3:   Documentación + deploy en homelab
  Día 4-5: Buffer para bugs de Windows/Tailscale
```

---

## Checklist de Entrega

- [ ] ttyd escucha en `0.0.0.0:7681` (no solo localhost)
- [ ] MCP SSE Relay expone `/sse` con EventSource compatible
- [ ] JWT usa `RS256` (no `HS256`), exp 2h, refresh 7d
- [ ] `alg: "none"` rechazado explícitamente en middleware
- [ ] `redirect_uri` validado contra whitelist exacta (no comodines)
- [ ] Cookies: `HttpOnly`, `Secure`, `SameSite=Strict`
- [ ] `overflow-x: hidden` en todos los viewports
- [ ] Contraste WCAG AA (4.5:1 texto, 3.0:1 títulos)
- [ ] Cero errores en consola JS
- [ ] Cero recursos rotos (404)
- [ ] Checkpoints persistidos en `.harness/checkpoints/`
- [ ] Resume funciona tras kill + reinicio
- [ ] Firewall Windows: solo puertos 7681, 8080, 8081 abiertos
- [ ] Tailscale Funnel configurado (HTTPS) o certificado propio
