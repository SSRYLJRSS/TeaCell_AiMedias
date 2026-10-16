param([Parameter(Mandatory=$true)][string]$Destination)
$ErrorActionPreference = 'Stop'
$taskRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$taskCache = [IO.Path]::GetFullPath((Join-Path $taskRoot 'artifacts/agent-runtime'))
$taskDestination = [IO.Path]::GetFullPath($Destination)
if (!$taskDestination.StartsWith($taskCache + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Stage outside artifact directory' }
if (Test-Path -LiteralPath $taskDestination) { throw 'Stage destination already exists' }
$taskManifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'runtime-manifest.json') -Raw | ConvertFrom-Json
$taskTarget = $taskManifest.targets.'x86_64-pc-windows-msvc'
$taskArchive = Join-Path $taskCache ([IO.Path]::GetFileName(([Uri]$taskTarget.url).AbsolutePath))
if ((Get-FileHash -LiteralPath $taskArchive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $taskTarget.sha256) { throw 'Runtime archive digest mismatch' }
Expand-Archive -LiteralPath $taskArchive -DestinationPath $taskDestination
