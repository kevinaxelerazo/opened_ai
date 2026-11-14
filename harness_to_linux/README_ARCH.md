# Opened-AI (`oai`) — Instalación rápida en Arch Linux (ThinkPad T480)

Paquete autocontenido del CLI/TUI `opened-ai` compilable de forma nativa en **Arch Linux**, apuntando de forma remota al servidor `llama-swap` (`:8080`) de tu PC con RTX 3060 Ti.

## 1. Requisitos previos en Arch Linux

En tu T480 ejecuta:

```bash
sudo pacman -S --needed base-devel rustup git ripgrep
rustup default stable
```

- **Opcional (solo si usarás `/vision` o `vision_inspect` headless desde la laptop):**
  ```bash
  sudo pacman -S --needed chromium
  ```
- **Nota TLS:** `opened-ai` usa `rustls` puro en Rust, así que no requiere `openssl` ni librerías C adicionales para compilar.

## 2. Compilar e instalar en `~/.local/bin/oai`

Copia esta carpeta `harness_to_linux/` a tu T480 (por `scp`, `rsync` o USB) y ejecuta dentro de ella:

```bash
chmod +x install.sh
./install.sh --server 192.168.18.10
```

- Reemplaza `192.168.18.10` (Ethernet LAN) por `100.118.52.119` si te conectas por **Tailscale** desde fuera de casa (o `192.168.18.20` si usas el Wi-Fi del servidor).
- El instalador hará automáticamente:
  1. Verificar `cargo`, `rustup default stable` y `gcc`/`base-devel`.
  2. Compilar `oai` nativamente en modo `--release`.
  3. Instalar el binario en `~/.local/bin/oai`.
  4. Copiar las 14 `skills/` y escribir `~/.config/opened-ai/config.yaml` apuntando al servidor (`http://<IP>:8080/v1`, presupuesto `65536 / 40960` tokens).

Si `~/.local/bin` aún no está en tu `PATH`, agrégalo en `~/.bashrc` o `~/.zshrc`:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

## 3. Usar en cualquier otro proyecto

Una vez instalado, puedes ir a cualquier carpeta de proyecto en tu T480 y lanzar:

```bash
cd ~/proyectos/mi_otro_proyecto
oai chat
```

Comandos útiles sin editar archivos:
- `oai status` — comprueba conectividad y modelos disponibles en el PC servidor.
- `oai agents` — lista los agentes configurados (`ti_manager`, `qwen38-uncensored`, `gemma4-26b`, etc.).
- Cambiar temporalmente de host entre LAN y Tailscale sin reinstalar:
  ```bash
  OPENED_AI_HOST=100.118.52.119 oai chat
  ```
  o bien:
  ```bash
  oai -s 100.118.52.119 chat
  ```

## 4. Firewall en el PC servidor (Windows) si no conecta por LAN

El `llama-swap.exe` ya escucha en `:::8080`. Si desde Arch `oai status` no alcanza el puerto `8080`, abre la regla de entrada TCP `8080` en Windows Firewall del PC servidor (en Perfil Privado / Tailscale).

