---
name: red-auth-jwt-audit
description: Auditoría profunda de Autenticación, Sesiones, JWT y flujos OAuth2/OIDC (inspirado en metodologías Claude-Red AppSec).
triggers:
  - jwt
  - oauth
  - auth
  - login
  - session
  - token
  - password
---

# 🔐 Skill: Auditoría de Identidad, JWT, OAuth2 y Sesiones

## 1. Vectores Críticos en JSON Web Tokens (JWT)
1. **Validación de Firma y Algoritmo:**
   - Verificar que el backend rechace explícitamente `alg: "none"`.
   - Comprobar si existe confusión de clave asimétrica/simétrica (`RS256` $\rightarrow$ `HS256` usando la clave pública como secreto HMAC).
   - Auditar secretos HMAC débiles o hardcodeados en el código o `.env`.
2. **Validación de Claims Obligatorios:**
   - Confirmar que se validen `exp` (expiración), `nbf`, `iss` (emisor) y `aud` (audiencia) antes de aceptar la identidad.

## 2. Flujos OAuth2 / OIDC y Gestión de Sesiones
- **Open Redirect & Robo de `code`:** Verificar que `redirect_uri` se valide contra una lista blanca exacta (sin comodines ni coincidencias parciales de dominio).
- **CSRF en OAuth:** Confirmar que el parámetro `state` sea criptográficamente aleatorio, esté vinculado a la sesión y se valide al recibir el callback.
- **Cookies de Sesión:** Exigir banderas `HttpOnly`, `Secure` y `SameSite=Strict` o `Lax`.
- **Hashing de Contraseñas:** Exigir `Argon2id`, `bcrypt` o `scrypt`; reportar como CRÍTICO cualquier uso de `MD5`, `SHA1` o `SHA256` sin sal ni factor de trabajo.

