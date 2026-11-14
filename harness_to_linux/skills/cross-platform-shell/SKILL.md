---
name: cross-platform-shell
description: "Reglas exactas de sintaxis para Windows PowerShell vs Linux Bash/sh, manejo de procesos, redes, Tailscale y creación de archivos."
triggers:
  - powershell
  - bash
  - linux
  - windows
  - shell
  - ttyd
  - tailscale
  - systemd
---

# 🖥️ Skill: Sintaxis Multiplataforma (Windows PowerShell + Linux Bash/sh)

## 1. Regla Universal de `opened-ai`
- `write_file` crea los directorios padre automáticamente (`fs::create_dir_all`). **NO** ejecutes `mkdir` previo en `shell_exec` solo para crear carpetas antes de escribir archivos.
- Verifica siempre el encabezado `[Entorno OS: ...]` del prompt antes de usar `shell_exec`.

## 2. Windows (`powershell -NoProfile -Command`)
- **Separador de comandos:** Usa `;` (punto y coma), **NUNCA** `&&` ni `||` (PowerShell 5.1 los rechaza con error de parser).
- **Directorios y listados:**
  - `New-Item -ItemType Directory -Force -Path "ruta/subcarpeta"`
  - `Get-ChildItem -Recurse -Depth 3 -File | Select-Object -First 40 FullName`
  - `Select-String -Path "src\**\*.rs" -Pattern "texto"`
- **Binarios y procesos:**
  - `where.exe cargo`, `Get-Command ttyd -ErrorAction SilentlyContinue`
  - `Get-NetTCPConnection -State Listen`
  - Reglas Firewall Windows: `netsh advfirewall firewall show rule name=all`
- **Background (`bg_start`):**
  - Evita envolver en bash (`nohup ... &`); `bg_start` ya lanza el proceso desacoplado y guarda logs en `.harness/bg_logs/`.

## 3. Linux / Unix (`sh -c` / `bash`)
- **Separador de comandos:** Puedes usar `&&`, `;`, o `||`.
- **Directorios y listados:**
  - `mkdir -p ruta/subcarpeta`
  - `find src -maxdepth 3 -type f` o `rg -n "texto" src/`
- **Binarios, puertos y servicios:**
  - `which ttyd oai cargo`
  - `ss -tulpn | grep -E '7681|8080|8081'`
  - Servicios: `systemctl --user status ...` o `sudo systemctl status ...`
  - Firewall Linux: `sudo ufw status` o `sudo iptables -L -n`

## 4. Portabilidad Rust (Windows + Linux)
- Para matar procesos hijos/árboles desde Rust de forma portable, no asumas señales POSIX (`SIGKILL`); usa `tokio::process::Child::kill().await` o `taskkill /T /F /PID` en `#[cfg(windows)]`.
- Para rutas, usa `std::path::PathBuf` / `join()`, nunca concatenaciones con `/` o `\` hardcodeados.

