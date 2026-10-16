param([string]$Target = 'x86_64-pc-windows-msvc', [switch]$DownloadOnly)
$ErrorActionPreference = 'Stop'
$taskManifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'runtime-manifest.json') -Raw | ConvertFrom-Json
$taskTarget = $taskManifest.targets.PSObject.Properties[$Target]
if ($null -eq $taskTarget) { throw "Unverified prototype runtime target: $Target" }
$taskSource = [Uri]$taskTarget.Value.url
if ($taskSource.Scheme -ne 'https' -or $taskSource.Host -ne 'nodejs.org') { throw 'Invalid runtime source' }
if ($taskTarget.Value.sha256 -notmatch '^[a-f0-9]{64}$') { throw 'Invalid runtime digest' }
$taskArchitecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$taskHostTarget = if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows) -and $taskArchitecture -eq 'X64') { 'x86_64-pc-windows-msvc' }
elseif ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::OSX) -and $taskArchitecture -eq 'Arm64') { 'aarch64-apple-darwin' }
elseif ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Linux) -and $taskArchitecture -eq 'X64') { 'x86_64-unknown-linux-gnu' }
else { $null }
if (!$DownloadOnly -and $taskHostTarget -ne $Target) { throw 'Runtime execution requires the matching native host; use DownloadOnly to verify a foreign archive' }
$taskRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$taskRuntimeDir = Join-Path $taskRoot 'artifacts/agent-runtime'
New-Item -ItemType Directory -Path $taskRuntimeDir -Force | Out-Null
$taskZip = Join-Path $taskRuntimeDir ([IO.Path]::GetFileName($taskSource.AbsolutePath))
if (!(Test-Path -LiteralPath $taskZip)) {
    Invoke-WebRequest -Uri $taskSource.AbsoluteUri -OutFile $taskZip
}
$taskActualHash = (Get-FileHash -LiteralPath $taskZip -Algorithm SHA256).Hash.ToLowerInvariant()
if ($taskActualHash -ne $taskTarget.Value.sha256) { throw 'Runtime SHA256 mismatch; refusing extraction' }
if ($DownloadOnly) { Write-Output $taskZip; exit 0 }
$taskExe = [IO.Path]::GetFullPath((Join-Path $taskRuntimeDir $taskTarget.Value.executable))
$taskAllowedPrefix = [IO.Path]::GetFullPath($taskRuntimeDir) + [IO.Path]::DirectorySeparatorChar
if (!$taskExe.StartsWith($taskAllowedPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Runtime path escapes cache' }
if ($taskZip.EndsWith('.zip', [StringComparison]::OrdinalIgnoreCase)) {
    Expand-Archive -LiteralPath $taskZip -DestinationPath $taskRuntimeDir -Force
} elseif ($taskZip.EndsWith('.tar.gz', [StringComparison]::OrdinalIgnoreCase)) {
    $taskTar = (Get-Command tar -CommandType Application -ErrorAction Stop).Source
    & $taskTar -xzf $taskZip -C $taskRuntimeDir
    if ($LASTEXITCODE -ne 0) { throw 'Runtime tar extraction failed' }
} else { throw 'Unsupported runtime archive format' }
$taskVersion = & $taskExe --version
if ($LASTEXITCODE -ne 0 -or $taskVersion -ne ('v' + $taskManifest.nodeVersion)) { throw 'Runtime version mismatch' }
Write-Output $taskExe
