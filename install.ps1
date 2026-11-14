param(
    [string]$ServerHost = "",
    [switch]$ForceConfig,
    [switch]$NonInteractive,
    [string]$BinDir = "$env:LOCALAPPDATA\Microsoft\WindowsApps",
    [string]$ConfigDir = "$env:LOCALAPPDATA\opened-ai"
)

$ErrorActionPreference = "Stop"
$RepoDir = Split-Path -Parent $MyInvocation.MyCommand.Path

function Normalize-ServerRoot([string]$Raw) {
    $clean = $Raw.Trim().TrimEnd('/')
    if ($clean.EndsWith('/v1')) {
        $clean = $clean.Substring(0, $clean.Length - 3)
    }
    if ([string]::IsNullOrWhiteSpace($clean)) {
        return "http://127.0.0.1:8080"
    }
    if ($clean -match '^https?://') {
        return $clean
    }
    if ($clean.Contains(':')) {
        return "http://$clean"
    }
    return "http://${clean}:8080"
}

Write-Host "==> [1/4] Verificando Rust / Cargo..." -ForegroundColor Cyan
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if (Test-Path (Join-Path $cargoBin "cargo.exe")) {
        $env:PATH = "$cargoBin;$env:PATH"
    }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "No se encontró 'cargo'. Instala Rust desde https://rustup.rs o con: winget install Rustlang.Rustup" -ForegroundColor Yellow
    exit 1
}

Write-Host "==> [2/4] Compilando opened-ai en modo release..." -ForegroundColor Cyan
cargo build --release --manifest-path (Join-Path $RepoDir "Cargo.toml")
if ($LASTEXITCODE -ne 0) {
    throw "Falló 'cargo build --release'."
}

Write-Host "==> [3/4] Instalando binario oai.exe en $BinDir..." -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
$ReleaseBin = Join-Path $RepoDir "target\release\oai.exe"
$DestBin = Join-Path $BinDir "oai.exe"
Copy-Item -Force $ReleaseBin $DestBin

Write-Host "==> [4/4] Configurando $ConfigDir (skills + config.yaml)..." -ForegroundColor Cyan
$DestSkills = Join-Path $ConfigDir "skills"
New-Item -ItemType Directory -Force -Path $DestSkills | Out-Null
Copy-Item -Recurse -Force (Join-Path $RepoDir "skills\*") $DestSkills

$TargetConfig = Join-Path $ConfigDir "config.yaml"
if ((-not (Test-Path $TargetConfig)) -or $ForceConfig -or (-not [string]::IsNullOrWhiteSpace($ServerHost))) {
    if ([string]::IsNullOrWhiteSpace($ServerHost) -and (-not $NonInteractive)) {
        $ServerHost = Read-Host "IP/host del PC servidor con llama-swap (Enter para 127.0.0.1:8080, ej. 192.168.18.10)"
    }
    if ([string]::IsNullOrWhiteSpace($ServerHost)) {
        $ServerHost = "127.0.0.1:8080"
    }
    $ServerRoot = Normalize-ServerRoot $ServerHost
    $Template = Get-Content -Raw -Encoding UTF8 (Join-Path $RepoDir "config\config.yaml")
    $Rendered = $Template.Replace("http://127.0.0.1:8080", $ServerRoot)
    Set-Content -Path $TargetConfig -Value $Rendered -Encoding UTF8
    Write-Host "Configuración escrita en: $TargetConfig -> $ServerRoot/v1" -ForegroundColor Green
} else {
    Write-Host "Se conserva configuración existente en: $TargetConfig (usa -ForceConfig o -ServerHost para actualizarla)." -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Instalación completada." -ForegroundColor Green
& $DestBin agents
Write-Host ""
Write-Host "Comprobando conexión al servidor LLM..." -ForegroundColor Cyan
try {
    & $DestBin status
} catch {
    Write-Host "Aviso: No se pudo conectar aún al servidor llama-swap. Verifica que esté activo y el puerto 8080 abierto." -ForegroundColor Yellow
}

