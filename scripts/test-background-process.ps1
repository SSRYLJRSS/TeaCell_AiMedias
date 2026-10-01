# Run on Windows: use a GUI-subsystem parent so a console child cannot inherit a console.
$ErrorActionPreference = "Stop"
if (-not $IsWindows) { throw "This regression requires Windows." }
$repoRoot = Split-Path $PSScriptRoot -Parent
$probeDir = Join-Path $repoRoot "artifacts/background-process-test"
New-Item -ItemType Directory -Force -Path $probeDir | Out-Null
$fixtureSource = @'
#[link(name = "kernel32")]
unsafe extern "system" { fn GetConsoleWindow() -> *mut core::ffi::c_void; }
#[link(name = "user32")]
unsafe extern "system" { fn IsWindowVisible(window: *mut core::ffi::c_void) -> i32; }
fn main() {
    let window = unsafe { GetConsoleWindow() };
    let visible = !window.is_null() && unsafe { IsWindowVisible(window) } != 0;
    println!("console={} visible={visible}", !window.is_null());
    if std::env::args().any(|arg| arg == "--slow") {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
}
'@
$parentSource = @'
#![windows_subsystem = "windows"]
#[path = "PROCESS_MODULE_PATH"]
mod process;
fn main() {
    let fixture = std::env::args_os().nth(1).expect("fixture path");
    let output = process::output_with_timeout(&mut process::background_command(&fixture), std::time::Duration::from_secs(5)).expect("spawn fixture");
    let report = String::from_utf8(output.stdout).expect("fixture report");
    print!("{report}");
    if !output.status.success() || report.trim() != "console=false visible=false" {
        eprintln!("FAIL: a background console was allocated");
        std::process::exit(1);
    }
    println!("PASS: background child has no console window");
    let started = std::time::Instant::now();
    let error = process::output_with_timeout(process::background_command(&fixture).arg("--slow"), std::time::Duration::from_millis(50)).expect_err("timeout");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    println!("PASS: stalled probe is terminated and reaped within timeout");
}
'@
$modulePath = (Join-Path $repoRoot "src-tauri/src/utils/process.rs").Replace('\', '/')
$parentSource = $parentSource.Replace('PROCESS_MODULE_PATH', $modulePath)
$fixturePath = Join-Path $probeDir "console-fixture.rs"
$parentPath = Join-Path $probeDir "gui-parent.rs"
$fixtureExe = Join-Path $probeDir "console-fixture.exe"
$parentExe = Join-Path $probeDir "gui-parent.exe"
Set-Content -LiteralPath $fixturePath -Value $fixtureSource -Encoding utf8
Set-Content -LiteralPath $parentPath -Value $parentSource -Encoding utf8
& rustc --edition 2021 $fixturePath -o $fixtureExe
if ($LASTEXITCODE -ne 0) { throw "Fixture compilation failed." }
& rustc --edition 2021 $parentPath -o $parentExe
if ($LASTEXITCODE -ne 0) { throw "Parent compilation failed." }
$reportPath = Join-Path $probeDir "report.txt"
$errorPath = Join-Path $probeDir "error.txt"
$probe = Start-Process -FilePath $parentExe -ArgumentList ('"' + $fixtureExe + '"') -WindowStyle Hidden -Wait -PassThru -RedirectStandardOutput $reportPath -RedirectStandardError $errorPath
Get-Content -LiteralPath $reportPath
if ($probe.ExitCode -ne 0) {
    Get-Content -LiteralPath $errorPath
    throw "Background console regression failed."
}
