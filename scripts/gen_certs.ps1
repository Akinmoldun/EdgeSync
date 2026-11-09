# ==============================================================================
# EdgeSync — mTLS Certificate Generator (PowerShell / Windows)
# Specification: §8 Week 10
#
# Generates:
# - ca.crt, ca.key       (Root Certificate Authority)
# - server.crt, server.key (Server cert with SAN: localhost, 127.0.0.1)
# - client.crt, client.key (Client cert with clientAuth EKU)
# ==============================================================================

param (
    [string]$OutputDir = ""
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$RootDir = Split-Path -Parent $ScriptDir

if (-not $OutputDir) {
    $OutputDir = Join-Path $RootDir "certs"
}

if (-not (Test-Path $OutputDir)) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}

Write-Host "Generating mTLS certificates in: $OutputDir" -ForegroundColor Cyan

# Use Go generator for 100% platform-independent x509 cert creation
Push-Location "$RootDir\server"
try {
    go run ./cmd/gen-certs -out "$OutputDir"
} finally {
    Pop-Location
}

if ((Test-Path (Join-Path $OutputDir "ca.crt")) -and
    (Test-Path (Join-Path $OutputDir "server.crt")) -and
    (Test-Path (Join-Path $OutputDir "client.crt"))) {
    Write-Host "✅ mTLS certificates successfully generated." -ForegroundColor Green
    exit 0
} else {
    Write-Host "❌ Failed to generate all required certificates." -ForegroundColor Red
    exit 1
}
