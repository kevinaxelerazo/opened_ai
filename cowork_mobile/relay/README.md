# MCP SSE Relay

## Descripción

Relay binario en Rust que:

1. **Lanza un proceso hijo** (ej. `lh.exe mcp`) con stdin/stdout pipes.
2. **Reenvía bidireccionalmente**: stdin → child_stdin y child_stdout → SSE.
3. **Exponen un endpoint SSE** en `:8080` para clientes móviles (EventSource).
4. **Reinicia automáticamente** el proceso hijo si muere (con delay de 2s, máx 10 intentos).

## Uso

```bash
# Con flag
cargo run -- --process-cmd "lh.exe mcp"

# Con variable de entorno
LH_PROCESS_CMD="lh.exe mcp" cargo run --release
```

## Endpoints

| Endpoint | Método | Descripción |
|----------|--------|-------------|
| `/sse` | GET | Server-Sent Events (JSON-RPC messages) |
| `/health` | GET | `{ "status": "ok", "pid": <pid>, "uptime": <secs> }` |

## Formato SSE

```
event: message
data: {"type":"message","data":"<raw stdout line>","timestamp":"now"}

event: connected
data: {"status":"connected"}
```

## Arquitectura

```
┌─────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  Móvil      │────▶│  SSE Relay :8080 │────▶│  lh.exe mcp    │
│  (EventSrc) │◀────│  (Rust binary)   │◀────│  (stdio child) │
└─────────────┘     └──────────────────┘     └─────────────────┘
```

## Dependencias

- `tokio` (full) — async runtime
- `serde_json` — serialización JSON
- `anyhow` — manejo de errores
- `tokio-util` — codec para líneas (LinesCodec)
- `uuid` — generación de IDs (si se extiende)

## Seguridad

- El relay **no expone shell directo** — solo reenvía stdout del proceso hijo.
- El proceso hijo se lanza con `stdin(Stdio::piped())` y `stdout(Stdio::piped())`.
- No se ejecuta como root (verificar con `USER nonroot` en systemd).
- Puerto `:8080` debe estar detrás de Tailscale mTLS o firewall.
