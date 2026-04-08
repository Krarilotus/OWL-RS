param(
    [string]$FusekiHome = (Join-Path (Resolve-Path "..").Path "Apache_Fuseki\apache-jena-fuseki-6.0.0"),
    [string]$DatasetPath = "/ds",
    [int]$NresePort = 18080,
    [int]$FusekiPort = 3031,
    [ValidateSet("full", "compat-only")]
    [string]$ExecutionMode = "full",
    [string]$Tier = "small",
    [string]$Ontology,
    [string]$SemanticDialect,
    [string]$ReasoningFeature,
    [string]$ServiceCoverage,
    [string]$ReportDir = "artifacts/local-fuseki-pack-matrix",
    [string]$ReasoningMode = "rules-mvp",
    [string]$ReasonerPreset = "bounded-owl",
    [string]$CargoTargetDir = "target-local-parity",
    [switch]$RefreshCatalog
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Test-ProcessAlive {
    param([System.Diagnostics.Process]$Process, [string]$Label)
    if ($Process.HasExited) {
        throw "$Label exited early with code $($Process.ExitCode)"
    }
}

function Wait-HttpOk {
    param(
        [string]$Uri,
        [int]$TimeoutSeconds,
        [string]$Label
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        try {
            $response = Invoke-WebRequest -Uri $Uri -Method Get -UseBasicParsing -TimeoutSec 3
            if ($response.StatusCode -ge 200 -and $response.StatusCode -lt 300) {
                return
            }
        } catch {
            Start-Sleep -Milliseconds 500
        }
    }

    throw "$Label did not become ready at $Uri within $TimeoutSeconds seconds"
}

function Assert-PortFree {
    param([int]$Port, [string]$Label)

    $listeners = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
    if ($null -ne $listeners) {
        throw "$Label port $Port is already in use. Choose another port or stop the conflicting process first."
    }
}

function Start-ProcessWithEnv {
    param(
        [string]$FilePath,
        [string[]]$ArgumentList,
        [string]$WorkingDirectory,
        [hashtable]$EnvironmentVariables,
        [string]$StdoutPath,
        [string]$StderrPath
    )

    $previous = @{}
    foreach ($name in $EnvironmentVariables.Keys) {
        $previous[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        [Environment]::SetEnvironmentVariable($name, $EnvironmentVariables[$name], "Process")
    }

    try {
        return Start-Process `
            -FilePath $FilePath `
            -ArgumentList $ArgumentList `
            -WorkingDirectory $WorkingDirectory `
            -RedirectStandardOutput $StdoutPath `
            -RedirectStandardError $StderrPath `
            -PassThru
    } finally {
        foreach ($name in $EnvironmentVariables.Keys) {
            [Environment]::SetEnvironmentVariable($name, $previous[$name], "Process")
        }
    }
}

$repoRoot = (Get-Location).Path
$reportRoot = Join-Path $repoRoot $ReportDir
$logDir = Join-Path $reportRoot "logs"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null

if ($NresePort -eq $FusekiPort) {
    throw "NRESE and Fuseki must not use the same port"
}
Assert-PortFree -Port $NresePort -Label "NRESE"
Assert-PortFree -Port $FusekiPort -Label "Fuseki"

$fusekiBat = Join-Path $FusekiHome "fuseki-server.bat"
if (-not (Test-Path $fusekiBat)) {
    throw "Fuseki launcher not found: $fusekiBat"
}

$nreseStdout = Join-Path $logDir "nrese.stdout.log"
$nreseStderr = Join-Path $logDir "nrese.stderr.log"
$fusekiStdout = Join-Path $logDir "fuseki.stdout.log"
$fusekiStderr = Join-Path $logDir "fuseki.stderr.log"

$nreseProcess = $null
$fusekiProcess = $null

try {
    $nreseEnv = @{
        "CARGO_TARGET_DIR" = $CargoTargetDir
        "NRESE_BIND_ADDR" = "127.0.0.1:$NresePort"
        "NRESE_STORE_MODE" = "in-memory"
        "NRESE_REASONING_MODE" = $ReasoningMode
        "NRESE_REASONER_RULES_MVP_PRESET" = $ReasonerPreset
        "NRESE_AUTH_MODE" = "none"
        "NRESE_SPARQL_PARSE_ERROR_PROFILE" = "plain-text"
        "NRESE_ENABLE_METRICS" = "true"
        "NRESE_AI_ENABLED" = "false"
    }
    $nreseProcess = Start-ProcessWithEnv `
        -FilePath "cargo" `
        -ArgumentList @("run", "-p", "nrese-server") `
        -WorkingDirectory $repoRoot `
        -EnvironmentVariables $nreseEnv `
        -StdoutPath $nreseStdout `
        -StderrPath $nreseStderr

    $fusekiArgs = @("--mem", "--update", "--localhost", "--ping", "--port", "$FusekiPort", $DatasetPath)
    $fusekiProcess = Start-Process `
        -FilePath $fusekiBat `
        -ArgumentList $fusekiArgs `
        -WorkingDirectory $FusekiHome `
        -RedirectStandardOutput $fusekiStdout `
        -RedirectStandardError $fusekiStderr `
        -PassThru

    Start-Sleep -Seconds 2
    Test-ProcessAlive -Process $nreseProcess -Label "NRESE server"
    Test-ProcessAlive -Process $fusekiProcess -Label "Fuseki server"

    Wait-HttpOk -Uri "http://127.0.0.1:$NresePort/readyz" -TimeoutSeconds 45 -Label "NRESE server"
    Wait-HttpOk -Uri "http://127.0.0.1:$FusekiPort/\$/ping" -TimeoutSeconds 45 -Label "Fuseki server"

    if ($RefreshCatalog) {
        $catalogArgs = @(
            "run",
            "--manifest-path", "benches/nrese-bench-harness/Cargo.toml",
            "--",
            "catalog-sync",
            "--tier", $Tier
        )
        cargo @catalogArgs
    }

    $packMatrixArgs = @(
        "run",
        "--manifest-path", "benches/nrese-bench-harness/Cargo.toml",
        "--",
        "pack-matrix",
        "--nrese-base-url", "http://127.0.0.1:$NresePort",
        "--fuseki-base-url", "http://127.0.0.1:$FusekiPort$DatasetPath",
        "--execution-mode", $ExecutionMode,
        "--tier", $Tier,
        "--report-dir", $ReportDir
    )

    if ($Ontology) {
        $packMatrixArgs += @("--ontology", $Ontology)
    }
    if ($SemanticDialect) {
        $packMatrixArgs += @("--semantic-dialect", $SemanticDialect)
    }
    if ($ReasoningFeature) {
        $packMatrixArgs += @("--reasoning-feature", $ReasoningFeature)
    }
    if ($ServiceCoverage) {
        $packMatrixArgs += @("--service-coverage", $ServiceCoverage)
    }

    cargo @packMatrixArgs
} finally {
    foreach ($process in @($nreseProcess, $fusekiProcess)) {
        if ($null -ne $process) {
            try {
                if (-not $process.HasExited) {
                    Stop-Process -Id $process.Id -Force
                }
            } catch {
            }
        }
    }
}
