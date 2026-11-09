# ==============================================================================
# EdgeSync — End-to-End Integration Test Suite (PowerShell / Windows) (§8 Week 10)
# Enforces SQLCipher Encrypted SQLite Database & Full mTLS Transport Security
# ==============================================================================

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$RootDir = Split-Path -Parent $ScriptDir

$ServerPort = if ($env:PORT) { $env:PORT } else { 8089 }
$ServerUrl = "https://127.0.0.1:$ServerPort"
$DbUser = if ($env:DB_USER) { $env:DB_USER } else { "postgres" }
$DbPass = if ($env:DB_PASSWORD) { $env:DB_PASSWORD } else { "postgrespassword" }
$DbName = if ($env:DB_NAME) { $env:DB_NAME } else { "edgesync" }
$DbKey = "e2e-sqlcipher-passphrase-secret-42"

$TempDir = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), "edgesync_e2e_" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $TempDir -Force | Out-Null
$CertsDir = Join-Path $TempDir "certs"
New-Item -ItemType Directory -Path $CertsDir -Force | Out-Null

$ServerProcess = $null

Write-Host "============================================================" -ForegroundColor Cyan
Write-Host " Starting EdgeSync End-to-End Integration Test (mTLS + SQLCipher)" -ForegroundColor Cyan
Write-Host " Temp Directory: $TempDir"
Write-Host " Server URL:     $ServerUrl"
Write-Host "============================================================"

function Cleanup {
    Write-Host "`n=== Cleaning up resources ===" -ForegroundColor Yellow
    if ($ServerProcess -and -not $ServerProcess.HasExited) {
        Write-Host "Stopping Go Sync Server (PID: $($ServerProcess.Id))..."
        Stop-Process -Id $ServerProcess.Id -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path $TempDir) {
        Write-Host "Removing temporary test files..."
        Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

try {
    # 1. Build Rust client binary if needed
    Write-Host "Ensuring Rust client binary is built..."
    $BinPath = Get-ChildItem -Path "$RootDir\client\target" -Recurse -Filter "edgesync.exe" -ErrorAction SilentlyContinue |
               Select-Object -First 1 -ExpandProperty FullName

    if (-not $BinPath) {
        Write-Host "Building client binary..."
        Push-Location "$RootDir\client"
        cargo build
        Pop-Location
        $BinPath = Get-ChildItem -Path "$RootDir\client\target" -Recurse -Filter "edgesync.exe" |
                   Select-Object -First 1 -ExpandProperty FullName
    }
    Write-Host "Using client binary: $BinPath"

    # 2. Generate local mTLS PKI
    Write-Host "Generating local mTLS certificates..."
    & "$RootDir\scripts\gen_certs.ps1" -OutputDir "$CertsDir"

    $CaCert = Join-Path $CertsDir "ca.crt"
    $ServerCert = Join-Path $CertsDir "server.crt"
    $ServerKey = Join-Path $CertsDir "server.key"
    $ClientCert = Join-Path $CertsDir "client.crt"
    $ClientKey = Join-Path $CertsDir "client.key"

    # 3. Build & Start Go Server in background with mTLS
    Write-Host "Building Go Sync Server binary..."
    $ServerBin = Join-Path $TempDir "api.exe"
    Push-Location "$RootDir\server"
    go build -o $ServerBin ./cmd/api
    Pop-Location

    Write-Host "Starting Go Sync Server on port $ServerPort with mTLS..."
    $env:PORT = "$ServerPort"
    $env:DB_HOST = "localhost"
    $env:DB_PORT = "5432"
    $env:DB_USER = "$DbUser"
    $env:DB_PASSWORD = "$DbPass"
    $env:DB_NAME = "$DbName"
    $env:MIGRATIONS_DIR = "$RootDir\server\migrations"
    $env:SERVER_CERT_FILE = "$ServerCert"
    $env:SERVER_KEY_FILE = "$ServerKey"
    $env:CA_CERT_FILE = "$CaCert"

    $ServerLog = Join-Path $TempDir "server.log"
    $ServerErrLog = Join-Path $TempDir "server_err.log"
    $ServerProcess = Start-Process -FilePath $ServerBin -WorkingDirectory "$RootDir\server" -RedirectStandardOutput $ServerLog -RedirectStandardError $ServerErrLog -PassThru

    Write-Host "Waiting for server to become responsive at $ServerUrl/healthz via client mTLS..."
    $ready = $false
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Seconds 1
        $hOutFile = Join-Path $TempDir "health_probe.out"
        $hErrFile = Join-Path $TempDir "health_probe.err"
        $hProc = Start-Process -FilePath $BinPath -ArgumentList @(
            "--ca-cert", $CaCert,
            "--client-cert", $ClientCert,
            "--client-key", $ClientKey,
            "health",
            "--server-url", $ServerUrl
        ) -RedirectStandardOutput $hOutFile -RedirectStandardError $hErrFile -NoNewWindow -Wait -PassThru

        if ($hProc.ExitCode -eq 0) {
            $ready = $true
            break
        }
    }

    if (-not $ready) {
        Write-Host "Server failed to start within 30 seconds." -ForegroundColor Red
        if (Test-Path $ServerLog) { Get-Content $ServerLog }
        if (Test-Path $ServerErrLog) { Get-Content $ServerErrLog }
        exit 1
    }
    Write-Host "Server is ready with mTLS!" -ForegroundColor Green

    # Setup client isolated storage
    $ClientDb = Join-Path $TempDir "client.db"
    $DeviceSigningKey = Join-Path $TempDir "device.key"

    function Run-Client {
        param([string[]]$ClientArgs)
        & $BinPath --db $ClientDb --key $DeviceSigningKey --db-key $DbKey --ca-cert $CaCert --client-cert $ClientCert --client-key $ClientKey @ClientArgs
    }

    # 4. Client init device with SQLCipher encryption key
    Write-Host "`n--- Step 1: Initializing device identity with SQLCipher ---"
    $initOut = Run-Client @("init-device") | Out-String
    Write-Host $initOut

    $devId = [regex]::Match($initOut, 'Device ID:\s+([a-f0-9\-]+)').Groups[1].Value
    $pubKeyHex = [regex]::Match($initOut, 'Public Key \(hex\):\s+([a-f0-9]+)').Groups[1].Value

    Write-Host "Device ID: $devId"
    Write-Host "Public Key: $pubKeyHex"

    # Register in Postgres via docker exec
    Write-Host "Registering device in PostgreSQL..."
    docker exec edgesync-postgres psql -U $DbUser -d $DbName -c "INSERT INTO devices (id, public_key) VALUES ('$devId', decode('$pubKeyHex', 'hex')) ON CONFLICT DO NOTHING;"

    # 5. Add revenue transactions locally
    $entityId1 = "rev-ps-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8)
    $entityId2 = "rev-ps-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8)

    Write-Host "`n--- Step 2: Adding local revenue events ---"
    Run-Client @("add-revenue", "--amount", "5000", "--description", "PS First Sale", "--entity-id", $entityId1) | Out-Null
    Run-Client @("add-revenue", "--amount", "2500", "--description", "PS Second Sale", "--entity-id", $entityId2) | Out-Null

    Write-Host "`n--- Step 3: Checking Outbox (expecting 2 pending) ---"
    $outbox = Run-Client @("outbox") | Out-String
    Write-Host $outbox
    if (-not ($outbox.Contains($entityId1) -and $outbox.Contains($entityId2))) {
        throw "Outbox did not contain expected events"
    }

    Write-Host "`n--- Step 4: Pushing to server over mTLS ---"
    $pushOut = Run-Client @("push", "--server-url", $ServerUrl) | Out-String
    Write-Host $pushOut
    if (-not ($pushOut -match "Accepted:\s+2")) {
        throw "Push did not report 2 accepted events"
    }

    $outboxDrained = Run-Client @("outbox") | Out-String
    if (-not ($outboxDrained -match "Pending Outbox Events \(0\)")) {
        throw "Outbox was not drained after push"
    }
    Write-Host "Outbox drained successfully." -ForegroundColor Green

    # Query latest server_seq in Postgres for isolated pull
    $maxSeq = (docker exec edgesync-postgres psql -U $DbUser -d $DbName -t -A -c "SELECT COALESCE(MAX(server_seq), 0) FROM events;").Trim()

    Write-Host "`n--- Step 5: Pulling from server (idle pull) over mTLS ---"
    $pullOut = Run-Client @("pull", "--server-url", $ServerUrl, "--after-seq", $maxSeq) | Out-String
    Write-Host $pullOut
    if (-not $pullOut.Contains("No new events to pull")) {
        throw "Expected no new events to pull"
    }

    Write-Host "`n--- Step 6: Simulating concurrent conflict ---"
    # Local client mutation
    Run-Client @("add-revenue", "--amount", "7500", "--description", "Local client mutation", "--entity-id", $entityId1) | Out-Null

    # Fake remote device direct insert into Postgres
    $remoteDev = [System.Guid]::NewGuid().ToString()
    $remoteEvent = [System.Guid]::NewGuid().ToString()
    $remoteHex = "00" * 32
    $sigHex = "00" * 64

    docker exec edgesync-postgres psql -U $DbUser -d $DbName -c "INSERT INTO devices (id, public_key) VALUES ('$remoteDev', decode('$remoteHex', 'hex')) ON CONFLICT DO NOTHING;"
    docker exec edgesync-postgres psql -U $DbUser -d $DbName -c "INSERT INTO events (id, device_id, entity_type, entity_id, event_type, payload, vector_clock, signature) VALUES ('$remoteEvent', '$remoteDev', 'revenue', '$entityId1', 'revenue.recorded', '{\`"amount_cents\`": 9100}', '{\`"$remoteDev\`": 1}', decode('$sigHex', 'hex'));"

    Write-Host "Client pulling to trigger conflict over mTLS..."
    $pullConfOut = Run-Client @("pull", "--server-url", $ServerUrl, "--after-seq", $maxSeq) | Out-String
    Write-Host $pullConfOut
    if (-not ($pullConfOut -match "Conflicts:\s+1")) {
        throw "Pull did not detect expected concurrent conflict"
    }
    Write-Host "Concurrent conflict detected as expected!" -ForegroundColor Green

    Write-Host "`n--- Step 7: Verifying un-synced local state was NOT overwritten ---"
    $showState = Run-Client @("show-state", "-t", "revenue", "-i", $entityId1) | Out-String
    Write-Host $showState
    if (-not $showState.Contains("7500")) {
        throw "Local state was overwritten by concurrent pull!"
    }
    Write-Host "Local un-synced state preserved correctly." -ForegroundColor Green

    Write-Host "`n--- Step 8: Listing conflicts over mTLS ---"
    $confOut = Run-Client @("conflicts", "--server-url", $ServerUrl) | Out-String
    Write-Host $confOut
    if (-not $confOut.Contains($entityId1)) {
        throw "Conflict listing did not surface entity conflict"
    }

    Write-Host "`n============================================================" -ForegroundColor Green
    Write-Host " ✅ ALL END-TO-END CHECKS PASSED (SQLCipher + mTLS)!" -ForegroundColor Green
    Write-Host "============================================================" -ForegroundColor Green
    exit 0
} catch {
    Write-Host "❌ Error: $_" -ForegroundColor Red
    exit 1
} finally {
    Cleanup
}
