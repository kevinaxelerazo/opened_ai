#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${HOME}/.local/bin"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/opened-ai"
SERVER_HOST=""
FORCE_CONFIG=0
NON_INTERACTIVE=0

usage() {
  cat <<'EOF'
Uso: ./install.sh [opciones]

Opciones:
  --server <IP[:PUERTO]>   IP o URL del PC servidor con llama-swap (ej: 192.168.18.10 o 192.168.18.10:8080)
  --bin-dir <RUTA>         Carpeta donde instalar el binario oai (default: ~/.local/bin)
  --config-dir <RUTA>      Carpeta de configuración y skills (default: ~/.config/opened-ai)
  --force-config           Sobrescribir ~/.config/opened-ai/config.yaml si ya existe
  -y, --non-interactive    No pedir datos interactivos
  -h, --help               Mostrar esta ayuda
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server)
      SERVER_HOST="${2:-}"
      shift 2
      ;;
    --bin-dir)
      BIN_DIR="${2:-}"
      shift 2
      ;;
    --config-dir)
      CONFIG_DIR="${2:-}"
      shift 2
      ;;
    --force-config)
      FORCE_CONFIG=1
      shift
      ;;
    -y|--non-interactive)
      NON_INTERACTIVE=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Opción desconocida: $1" >&2
      usage
      exit 1
      ;;
  esac
done

normalize_server_root() {
  local raw="$1"
  raw="${raw%/}"
  raw="${raw%/v1}"
  if [[ -z "$raw" ]]; then
    echo "http://127.0.0.1:8080"
    return
  fi
  if [[ "$raw" =~ ^https?:// ]]; then
    echo "$raw"
  elif [[ "$raw" == *:* ]]; then
    echo "http://$raw"
  else
    echo "http://$raw:8080"
  fi
}

echo "==> [1/4] Verificando toolchain de Rust (cargo)..."
if ! command -v cargo >/dev/null 2>&1; then
  if [[ -f "${HOME}/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    source "${HOME}/.cargo/env"
  fi
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "No se encontró 'cargo'. En Arch Linux puedes instalar:"
  echo "  sudo pacman -S --needed base-devel rustup git"
  echo "  rustup default stable"
  echo "O usar rustup oficial: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  if [[ "$NON_INTERACTIVE" -eq 0 ]]; then
    read -r -p "¿Quieres instalar Rustup automáticamente ahora? [s/N]: " ans
    if [[ "$ans" =~ ^[sSyY]$ ]]; then
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
      # shellcheck disable=SC1091
      source "${HOME}/.cargo/env"
    else
      exit 1
    fi
  else
    exit 1
  fi
fi

# En Arch Linux, el paquete 'rustup' instala /usr/bin/cargo sin toolchain por defecto hasta ejecutar 'rustup default stable'.
if command -v rustup >/dev/null 2>&1; then
  if ! rustup show active-toolchain >/dev/null 2>&1; then
    echo "Configurando toolchain 'stable' en rustup..."
    rustup toolchain install stable
    rustup default stable
  fi
fi

if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
  echo "Error: No se detectó compilador C (cc/gcc/clang). En Arch Linux instala: sudo pacman -S --needed base-devel" >&2
  exit 1
fi

echo "==> [2/4] Compilando opened-ai en modo release..."
cargo build --release --manifest-path "${REPO_DIR}/Cargo.toml"

echo "==> [3/4] Instalando binario en ${BIN_DIR}/oai..."
mkdir -p "$BIN_DIR"
cp "${REPO_DIR}/target/release/oai" "${BIN_DIR}/oai"
chmod +x "${BIN_DIR}/oai"

echo "==> [4/4] Configurando ${CONFIG_DIR} (skills + config.yaml)..."
mkdir -p "${CONFIG_DIR}/skills"
cp -R "${REPO_DIR}/skills/." "${CONFIG_DIR}/skills/"

TARGET_CONFIG="${CONFIG_DIR}/config.yaml"
if [[ ! -f "$TARGET_CONFIG" || "$FORCE_CONFIG" -eq 1 || -n "$SERVER_HOST" ]]; then
  if [[ -z "$SERVER_HOST" && "$NON_INTERACTIVE" -eq 0 ]]; then
    echo ""
    read -r -p "IP/host del PC servidor con llama-swap (Enter para usar 127.0.0.1:8080, ej. 192.168.18.10): " SERVER_HOST
  fi
  SERVER_ROOT="$(normalize_server_root "${SERVER_HOST:-127.0.0.1:8080}")"
  sed "s|http://127.0.0.1:8080|${SERVER_ROOT}|g" "${REPO_DIR}/config/config.yaml" > "$TARGET_CONFIG"
  echo "Configuración escrita en: ${TARGET_CONFIG} -> ${SERVER_ROOT}/v1"
else
  echo "Se conserva configuración existente en: ${TARGET_CONFIG} (usa --force-config o --server para actualizarla)."
fi

echo ""
echo "Instalación completada."
case ":$PATH:" in
  *":${BIN_DIR}:"*) ;;
  *)
    echo "IMPORTANTE: Agrega ${BIN_DIR} a tu PATH (por ejemplo en ~/.bashrc o ~/.zshrc):"
    echo "  export PATH=\"${BIN_DIR}:\$PATH\""
    ;;
esac

echo ""
"${BIN_DIR}/oai" agents || true
echo ""
echo "Comprobando conexión al servidor LLM (${TARGET_CONFIG})..."
"${BIN_DIR}/oai" status || echo "Aviso: No se pudo conectar aún al servidor llama-swap. Verifica que esté corriendo y el puerto 8080 abierto."

