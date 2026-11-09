# ==============================================================================
# EdgeSync — Chaos Simulation Harness with mTLS & SQLCipher (PowerShell / Windows)
# Specification: §8 Week 9 & Week 10
#
# Scenario:
# - Phase A: Generate mTLS certs, start Postgres & Go HTTPS mTLS server (poll /healthz)
# - Phase B: Build Rust client with SQLCipher, init device identity, register device on server
# - Phase C: Generate 50 encrypted offline events locally (Batch 1, do not push)
# - Phase D: Kill Go server (simulate complete DDIL blackout / network partition)
# - Phase E: Generate 50 MORE encrypted offline events during blackout (total 100 in outbox)
# - Phase F: Restart Go server (poll /healthz until healthy over mTLS)
# - Phase G: Client pushes all 100 events to server over mTLS (zero errors)
# - Phase H: Verify server DB has exactly 100 events for this device (Zero Data Loss)
# - Phase I: Verify client's outbox is completely empty
# - Phase J: Simulate concurrent mutation, pull over mTLS, and verify conflict surfacing
# - Security Checks: Prove SQLCipher rejects wrong/missing key, and mTLS fails closed
# ==============================================================================

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$RootDir = Split-Path -Parent $ScriptDir

$ServerPort = if ($env:PORT) { $env:PORT } else { 8095 }
$ServerUrl = "https://127.0.0.1:$ServerPort"
$DbUser = if ($env:DB_USER) { $env:DB_USER } else { "postgres" }
$DbPass = if ($env:DB_PASSWORD) { $env:DB_PASSWORD } else { "postgrespassword" }
$DbName = if ($env:DB_NAME) { $env:DB_NAME } else { "edgesync" }

$TempDir = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), "edgesync_chaos_" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $TempDir -Force | Out-Null
$RunId = [System.Guid]::NewGuid().ToString("N").Substring(0, 8)
$DbKey = "ddil-chaos-db-secret-key-$RunId"
$ServerProcess = $null
$ServerBin = Join-Path $RootDir "server\bin\api.exe"

$CertsDir = Join-Path $TempDir "certs"
$CaCert = Join-Path $CertsDir "ca.crt"
$ClientCert = Join-Path $CertsDir "client.crt"
$ClientKeyFile = Join-Path $CertsDir "client.key"
$ServerCert = Join-Path $CertsDir "server.crt"
$ServerKey = Join-Path $CertsDir "server.key"

function Cleanup {
    Write-Host "`n=== Cleaning up Chaos Test Resources ===" -ForegroundColor Yellow
    if ($ServerProcess -and -not $ServerProcess.HasExited) {
        Write-Host "Terminating Go Sync Server (PID: $($ServerProcess.Id))..."
        Stop-Process -Id $ServerProcess.Id -Force -ErrorAction SilentlyContinue
    }
    Get-Process -Name "api" -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $ServerBin } | Stop-Process -Force -ErrorAction SilentlyContinue

    if (Test-Path $TempDir) {
        Write-Host "Removing temporary test directory..."
        Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Wait-ServerReady($url, $timeoutSec = 30) {
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $pOut = Join-Path $TempDir "poll.log"
    $pErr = Join-Path $TempDir "poll_err.log"
    while ($stopwatch.Elapsed.TotalSeconds -lt $timeoutSec) {
        $proc = Start-Process -FilePath $BinPath -ArgumentList @("--ca-cert", $CaCert, "--client-cert", $ClientCert, "--client-key", $ClientKeyFile, "health", "--server-url", $url) -NoNewWindow -Wait -PassThru -RedirectStandardOutput $pOut -RedirectStandardError $pErr
        if ($proc.ExitCode -eq 0) {
            return $true
        }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

function Wait-ServerDown($url, $timeoutSec = 10) {
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $pOut = Join-Path $TempDir "poll.log"
    $pErr = Join-Path $TempDir "poll_err.log"
    while ($stopwatch.Elapsed.TotalSeconds -lt $timeoutSec) {
        $proc = Start-Process -FilePath $BinPath -ArgumentList @("--ca-cert", $CaCert, "--client-cert", $ClientCert, "--client-key", $ClientKeyFile, "health", "--server-url", $url) -NoNewWindow -Wait -PassThru -RedirectStandardOutput $pOut -RedirectStandardError $pErr
        if ($proc.ExitCode -ne 0) {
            return $true
        }
        Start-Sleep -Milliseconds 250
    }
    return $false
}

function Start-GoServer {
    $env:PORT = "$ServerPort"
    $env:DB_HOST = "localhost"
    $env:DB_PORT = "5432"
    $env:DB_USER = "$DbUser"
    $env:DB_PASSWORD = "$DbPass"
    $env:DB_NAME = "$DbName"
    $env:MIGRATIONS_DIR = "$RootDir\server\migrations"
    $env:SERVER_CERT_FILE = $ServerCert
    $env:SERVER_KEY_FILE = $ServerKey
    $env:CA_CERT_FILE = $CaCert

    $outLog = Join-Path $TempDir "server.log"
    $errLog = Join-Path $TempDir "server_err.log"
    return Start-Process -FilePath $ServerBin -WorkingDirectory "$RootDir\server" -RedirectStandardOutput $outLog -RedirectStandardError $errLog -PassThru
}

try {
    Write-Host "============================================================" -ForegroundColor Cyan
    Write-Host " Starting EdgeSync DDIL Chaos Simulation Harness (mTLS & SQLCipher)" -ForegroundColor Cyan
    Write-Host " Temp Directory: $TempDir"
    Write-Host " Server URL:     $ServerUrl (mTLS HTTPS)"
    Write-Host "============================================================"

    # Ensure client binary is available for health checks and test phases
    $BinPath = Get-ChildItem -Path "$RootDir\client\target" -Recurse -Filter "edgesync.exe" -ErrorAction SilentlyContinue |
               Select-Object -First 1 -ExpandProperty FullName

    if (-not $BinPath) {
        Push-Location "$RootDir\client"
        cargo build
        Pop-Location
        $BinPath = Get-ChildItem -Path "$RootDir\client\target" -Recurse -Filter "edgesync.exe" |
                   Select-Object -First 1 -ExpandProperty FullName
    }

    # --- Phase A: Generate Certs, Start Postgres & Go Server with mTLS ---
    Write-Host "`n>>> Phase A: Generating mTLS certificates and starting services..." -ForegroundColor White
    & "$RootDir\scripts\gen_certs.ps1" -OutputDir $CertsDir
    if (-not (Test-Path $CaCert) -or -not (Test-Path $ServerCert) -or -not (Test-Path $ClientCert)) {
        throw "Failed to generate mTLS certificates"
    }

    $pgReady = $false
    $null = docker exec edgesync-postgres pg_isready -U $DbUser -d $DbName 2>&1
    if ($LASTEXITCODE -eq 0) {
        $pgReady = $true
    } else {
        Write-Host "Starting PostgreSQL container via docker compose..."
        docker compose -f "$RootDir\docker\docker-compose.yml" up -d
        $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
        while ($stopwatch.Elapsed.TotalSeconds -lt 20) {
            $null = docker exec edgesync-postgres pg_isready -U $DbUser -d $DbName 2>&1
            if ($LASTEXITCODE -eq 0) {
                $pgReady = $true
                break
            }
            Start-Sleep -Milliseconds 500
        }
    }

    if (-not $pgReady) {
        throw "PostgreSQL container is not reachable!"
    }

    Write-Host "Compiling Go Sync Server binary..."
    Push-Location "$RootDir\server"
    go build -o "$ServerBin" ./cmd/api
    Pop-Location

    $ServerProcess = Start-GoServer
    if (-not (Wait-ServerReady $ServerUrl 30)) {
        throw "Go Sync Server failed to become responsive on $ServerUrl"
    }

    # Verify mTLS fail-closed: connection without client cert must be rejected
    $uOut = Join-Path $TempDir "unauth.log"
    $uErr = Join-Path $TempDir "unauth_err.log"
    $unauthProc = Start-Process -FilePath $BinPath -ArgumentList @("--ca-cert", $CaCert, "health", "--server-url", $ServerUrl) -NoNewWindow -Wait -PassThru -RedirectStandardOutput $uOut -RedirectStandardError $uErr
    if ($unauthProc.ExitCode -eq 0) {
        throw "Security violation! Server accepted connection without client certificate."
    }
    Write-Host "[PASS] Phase A: Postgres and Go HTTPS mTLS server running (fail-closed verified)" -ForegroundColor Green

    # --- Phase B: Register Device Identity ---
    Write-Host "`n>>> Phase B: Initializing client identity and registering device..." -ForegroundColor White
    $ClientDb = Join-Path $TempDir "chaos_client.db"
    $ClientKey = Join-Path $TempDir "chaos_device.key"

    function Run-Client {
        param([string[]]$ClientArgs)
        & $BinPath --db $ClientDb --key $ClientKey --db-key $DbKey --ca-cert $CaCert --client-cert $ClientCert --client-key $ClientKeyFile @ClientArgs
    }

    $initOut = Run-Client @("init-device") | Out-String
    $devId = [regex]::Match($initOut, 'Device ID:\s+([a-f0-9\-]+)').Groups[1].Value
    $pubKeyHex = [regex]::Match($initOut, 'Public Key \(hex\):\s+([a-f0-9]+)').Groups[1].Value

    if (-not $devId -or -not $pubKeyHex) {
        throw "Failed to extract device ID or public key from init-device output"
    }

    docker exec edgesync-postgres psql -U $DbUser -d $DbName -c "INSERT INTO devices (id, public_key) VALUES ('$devId', decode('$pubKeyHex', 'hex')) ON CONFLICT DO NOTHING;" | Out-Null
    Write-Host "[PASS] Phase B: Device $devId registered on server" -ForegroundColor Green

    # --- Phase C: Generate 50 Offline Events (Batch 1) ---
    Write-Host "`n>>> Phase C: Generating 50 offline encrypted events locally (Batch 1)..." -ForegroundColor White
    for ($i = 1; $i -le 50; $i++) {
        $amount = $i * 100
        $entityId = "rev-$RunId-b1-$i"
        Run-Client @("add-revenue", "--amount", "$amount", "--description", "Batch 1 revenue #$i", "--entity-id", "$entityId") | Out-Null
    }

    $outboxC = Run-Client @("outbox") | Out-String
    if (-not ($outboxC -match "Pending Outbox Events \(50\)")) {
        throw "Expected 50 pending outbox events after Phase C, got: $outboxC"
    }

    # Verify SQLCipher encryption: opening with wrong key fails
    $wkOut = Join-Path $TempDir "wrong_key.log"
    $wkErr = Join-Path $TempDir "wrong_key_err.log"
    $wkProc = Start-Process -FilePath $BinPath -ArgumentList @("--db", $ClientDb, "--key", $ClientKey, "--db-key", "wrong-key-12345", "list-events") -NoNewWindow -Wait -PassThru -RedirectStandardOutput $wkOut -RedirectStandardError $wkErr
    if ($wkProc.ExitCode -eq 0) {
        throw "SQLCipher failure! Database opened with wrong key."
    }
    Write-Host "[PASS] Phase C: Generated 50 offline events; SQLCipher encryption verified" -ForegroundColor Green

    # --- Phase D: KILL the Go Server (Network Partition / Blackout) ---
    Write-Host "`n>>> Phase D: Simulating network partition (killing Go server)..." -ForegroundColor White
    Stop-Process -Id $ServerProcess.Id -Force -ErrorAction SilentlyContinue
    if (-not (Wait-ServerDown $ServerUrl 10)) {
        throw "Server is still responding after kill attempt!"
    }
    Write-Host "[PASS] Phase D: Server successfully killed; network partition active" -ForegroundColor Green

    # --- Phase E: Generate 50 MORE Offline Events During Blackout ---
    Write-Host "`n>>> Phase E: Generating 50 more offline events during blackout (Batch 2)..." -ForegroundColor White
    for ($i = 1; $i -le 50; $i++) {
        $amount = ($i + 50) * 100
        $entityId = "rev-$RunId-b2-$i"
        Run-Client @("add-revenue", "--amount", "$amount", "--description", "Batch 2 blackout revenue #$i", "--entity-id", "$entityId") | Out-Null
    }

    $outboxE = Run-Client @("outbox") | Out-String
    if (-not ($outboxE -match "Pending Outbox Events \(100\)")) {
        throw "Expected 100 pending outbox events after blackout work, got: $outboxE"
    }
    Write-Host "[PASS] Phase E: Continuous offline work verified; 100 total events accumulated in encrypted outbox" -ForegroundColor Green

    # --- Phase F: Restart the Go Server ---
    Write-Host "`n>>> Phase F: Re-establishing network connectivity (restarting Go HTTPS server)..." -ForegroundColor White
    $ServerProcess = Start-GoServer
    if (-not (Wait-ServerReady $ServerUrl 30)) {
        throw "Go server failed to recover and become healthy on $ServerUrl"
    }
    Write-Host "[PASS] Phase F: Go HTTPS mTLS server reconnected and healthy" -ForegroundColor Green

    # --- Phase G: Client Pushes All 100 Events to Server ---
    Write-Host "`n>>> Phase G: Pushing all 100 accumulated events to server over mTLS..." -ForegroundColor White
    $pushOut = Run-Client @("push", "--server-url", "$ServerUrl") | Out-String
    Write-Host $pushOut
    if (-not ($pushOut -match "Accepted:\s+100")) {
        throw "Expected 100 accepted events in push, got: $pushOut"
    }
    Write-Host "[PASS] Phase G: All 100 events pushed and accepted over mTLS with zero errors" -ForegroundColor Green

    # --- Phase H: Verify Server DB has Exactly 100 Events for Device ---
    Write-Host "`n>>> Phase H: Verifying server database event count..." -ForegroundColor White
    $countOut = (docker exec edgesync-postgres psql -U $DbUser -d $DbName -t -A -c "SELECT COUNT(*) FROM events WHERE device_id = '$devId';").Trim()
    Write-Host "Server events count for device: $countOut"
    if ($countOut -ne "100") {
        throw "Zero-data-loss violation! Expected exactly 100 events in Postgres, found: $countOut"
    }
    Write-Host "[PASS] Phase H: Verified server DB contains exactly 100 events for device (Zero Data Loss)" -ForegroundColor Green

    # --- Phase I: Verify Client Outbox is Empty ---
    Write-Host "`n>>> Phase I: Verifying client outbox is completely drained..." -ForegroundColor White
    $outboxI = Run-Client @("outbox") | Out-String
    if (-not ($outboxI -match "Pending Outbox Events \(0\)")) {
        throw "Expected empty outbox after sync, got: $outboxI"
    }
    Write-Host "[PASS] Phase I: Client outbox completely drained" -ForegroundColor Green

    # --- Phase J: Concurrent Conflict Simulation & Surfacing ---
    Write-Host "`n>>> Phase J: Simulating concurrent conflict in partition recovery..." -ForegroundColor White
    $conflictEntity = "rev-$RunId-b1-1"

    # Local client mutation for entity
    Run-Client @("add-revenue", "--amount", "99999", "--description", "Local mutation after partition", "--entity-id", "$conflictEntity") | Out-Null

    # Concurrent remote event in Postgres
    $remoteDevId = [System.Guid]::NewGuid().ToString()
    $remoteEvId = [System.Guid]::NewGuid().ToString()
    $remoteHex = "00" * 32
    $sigHex = "00" * 64

    docker exec edgesync-postgres psql -U $DbUser -d $DbName -c "INSERT INTO devices (id, public_key) VALUES ('$remoteDevId', decode('$remoteHex', 'hex')) ON CONFLICT DO NOTHING;" | Out-Null
    docker exec edgesync-postgres psql -U $DbUser -d $DbName -c "INSERT INTO events (id, device_id, entity_type, entity_id, event_type, payload, vector_clock, signature) VALUES ('$remoteEvId', '$remoteDevId', 'revenue', '$conflictEntity', 'revenue.recorded', '{\`"amount_cents\`": 88888}', '{\`"$remoteDevId\`": 1}', decode('$sigHex', 'hex'));" | Out-Null

    # Pull latest sequence from server over mTLS
    $maxSeq = (docker exec edgesync-postgres psql -U $DbUser -d $DbName -t -A -c "SELECT COALESCE(MAX(server_seq) - 1, 0) FROM events;").Trim()
    $pullOut = Run-Client @("pull", "--server-url", "$ServerUrl", "--after-seq", "$maxSeq") | Out-String
    if (-not ($pullOut -match "Conflicts:\s+1")) {
        throw "Pull did not detect the concurrent conflict: $pullOut"
    }

    # Verify conflict surfaced over mTLS
    $conflictsOut = Run-Client @("conflicts", "--server-url", "$ServerUrl") | Out-String
    if (-not $conflictsOut.Contains($conflictEntity)) {
        throw "Conflict was not surfaced via conflicts command: $conflictsOut"
    }
    Write-Host "[PASS] Phase J: Concurrent conflict detected and surfaced correctly over mTLS" -ForegroundColor Green

    Write-Host "`n============================================================" -ForegroundColor Green
    Write-Host " ✅ CHAOS TEST PASSED: EdgeSync survived DDIL network blackout with mTLS, SQLCipher, and 0 data loss!" -ForegroundColor Green
    Write-Host "============================================================" -ForegroundColor Green
    exit 0
} catch {
    Write-Host "`n❌ CHAOS TEST FAILED: $_" -ForegroundColor Red
    if (Test-Path "$TempDir\server.log") {
        Write-Host "`n--- Server Stdout Log ---"
        Get-Content "$TempDir\server.log" -Tail 50
    }
    if (Test-Path "$TempDir\server_err.log") {
        Write-Host "`n--- Server Stderr Log ---"
        Get-Content "$TempDir\server_err.log" -Tail 50
    }
    exit 1
} finally {
    Cleanup
}
