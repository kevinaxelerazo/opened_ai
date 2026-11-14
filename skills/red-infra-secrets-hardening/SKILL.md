---
name: red-infra-secrets-hardening
description: Auditoría de infraestructura, contenedores Docker, proxies inversos (Caddy/Nginx), permisos y fuga de secretos.
triggers:
  - infra
  - docker
  - caddy
  - nginx
  - env
  - secret
  - port
  - systemd
  - servicio
---

# 🏰 Skill: Auditoría de Infraestructura, Contenedores y Fuga de Secretos

## 1. Escaneo de Secretos y Variables de Entorno
- Buscar con `search_code` patrones de claves expuestas: `API_KEY`, `SECRET`, `PASSWORD`, `TOKEN`, `PRIVATE_KEY`, `Bearer `, `ghp_`, `sk-`, cadenas de conexión con credenciales embebidas.
- Verificar que `.gitignore` excluya `.env`, `*.pem`, `*.key`, `id_rsa` y archivos de base de datos locales (`.sqlite`, `.db`).

## 2. Seguridad en Contenedores y Servicios Locales
- **Dockerfiles:** Verificar que no corran como `root` (`USER nonroot`), que no inyecten secretos en `ENV` o `ARG` durante el build, y que fijen versiones exactas de imágenes base.
- **Binding de Puertos:** Verificar si los servicios internos escuchan en `127.0.0.1` en lugar de `0.0.0.0` cuando deben estar detrás de un proxy inverso (Caddy/Nginx) o túnel (Tailscale).
- **Cabeceras de Seguridad HTTP:** Verificar presencia de `Content-Security-Policy (CSP)`, `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy` y CORS restringido a orígenes explícitos (nunca `Access-Control-Allow-Origin: *` con credenciales).

