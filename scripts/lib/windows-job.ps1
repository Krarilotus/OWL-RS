# The merge check's Windows step on its host (scripts/lib/fleet-gate.sh windows submits it): runs
# scripts/lib/windows-job.sh under Git Bash (by name, Windows finds WSL's bash.exe first), from a
# copy in the checkout's tmp/windows-job, since that script checks another commit out here.
#
#   powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/lib/windows-job.ps1 COMMIT TREE
#
# Exit: windows-job.sh's; 3 when there is no Git Bash or no copy (no verdict).
param([Parameter(Mandatory = $true)][string]$Commit, [Parameter(Mandatory = $true)][string]$Tree)

$git = Get-Command git.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
$roots = @('C:/Program Files/Git')
if ($git) { $roots = @(Split-Path (Split-Path $git.Source)) + $roots }
$bash = $roots | ForEach-Object { "$_/bin/bash.exe"; "$_/usr/bin/bash.exe" } |
    Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $bash) { Write-Output 'windows-job: no Git Bash on this host (WSL''s bash is not the Windows test host)'; exit 3 }
$env:PATH = (Join-Path $env:USERPROFILE '.cargo/bin') + ';' + (Split-Path $bash) + ';' +
    (Join-Path (Split-Path (Split-Path $bash)) 'usr/bin') + ';' + $env:PATH
try {
    New-Item -ItemType Directory -Force -Path 'tmp/windows-job' -ErrorAction Stop | Out-Null
    Copy-Item -LiteralPath 'scripts/lib/windows-job.sh' -Destination 'tmp/windows-job/windows-job.sh' -Force -ErrorAction Stop
} catch { Write-Output "windows-job: no copy of scripts/lib/windows-job.sh: $_"; exit 3 }
& $bash --noprofile --norc 'tmp/windows-job/windows-job.sh' $Commit $Tree
exit $LASTEXITCODE
