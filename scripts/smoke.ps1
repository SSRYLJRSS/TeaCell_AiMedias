# ============================================================
# Local P0 smoke suite (Windows) - equivalent to .github/workflows/smoke.yml
# Usage: pwsh ./scripts/smoke.ps1
# Note: keep this file ASCII-only comments; PowerShell 5.1 parses
# BOM-less UTF-8 as ANSI and garbles CJK comments (parser errors).
# Explicit `exit $LASTEXITCODE` avoids pwsh misreporting cargo's stderr
# progress as a non-zero exit (see docs/TEST_STRATEGY.md).
# Flaky handling: network integration tests (local TCP mock) occasionally
# fail once with "error sending request" on Windows (environment-level flaky)
# -> run serialized with --test-threads=1
# and retry once on failure; the stable group runs in parallel.
# ============================================================
$ErrorActionPreference = "Continue" # keep native stderr (cargo progress) from aborting steps
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Run-Step([string]$desc, [scriptblock]$block) {
    Write-Host "=== $desc ===" -ForegroundColor Cyan
    & $block
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAIL: $desc" -ForegroundColor Red
        exit $LASTEXITCODE
    }
}

function Invoke-NetworkTarget([string]$target) {
    Write-Host ">>> $target (serial, retry once on flaky)" -ForegroundColor DarkCyan
    cargo test --manifest-path src-tauri/Cargo.toml --test $target -- --test-threads=1
    if ($LASTEXITCODE -ne 0) {
        Write-Host ">>> $target first run failed (local TCP flaky), retry once" -ForegroundColor Yellow
        cargo test --manifest-path src-tauri/Cargo.toml --test $target -- --test-threads=1
    }
    if ($LASTEXITCODE -ne 0) { Write-Host "FAIL: $target" -ForegroundColor Red; exit $LASTEXITCODE }
}

# Cargo/heif-rs must use the SHA256-verified, target-specific local cache; never fall back to implicit downloads.
node scripts/prepare-heif-libraries.mjs --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$env:HEIF_BINARIES_DIR = (Resolve-Path "src-tauri/native/heif/x86_64-pc-windows-msvc").Path

Run-Step "Windows background console regression" {
    pwsh ./scripts/test-background-process.ps1
}

# [1/5] Backend stable group (parallel; network targets excluded to avoid flaky)
Run-Step "[1/5] cargo test stable group (lib + db_integration + qa_edge + format_matrix + services_integration)" {
    cargo test --manifest-path src-tauri/Cargo.toml `
        --lib `
        --test db_integration `
        --test qa_edge_tests `
        --test format_matrix `
        --test services_integration
}

# [2/5] Network group: AI / Ollama service-level integration (serial + retry)
Run-Step "[2/5] network integration tests (ai_service_integration / ollama_service_integration)" {
    Invoke-NetworkTarget "ai_service_integration"
    Invoke-NetworkTarget "ollama_service_integration"
}

Run-Step "[3/5] npm run typecheck" { npm run typecheck }
Run-Step "[4/5] npm run build" { npm run build }

if (Test-Path package.json) {
    $pkg = Get-Content package.json -Raw | ConvertFrom-Json
    if ($pkg.scripts."test:unit") {
        Run-Step "[5/5] npm run test:unit" { npm run test:unit }
    } else {
        Write-Host "=== [5/5] skip test:unit (vitest not configured yet) ===" -ForegroundColor DarkGray
    }
}

Write-Host "`n=== P0 smoke suite ALL PASS ===" -ForegroundColor Green
exit 0
