# The merge check's Windows step on its host (scripts/lib/fleet-gate.sh windows submits it): runs
# scripts/lib/windows-job.sh under Git Bash (by name, Windows finds WSL's bash.exe first).
#
#   powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/lib/windows-job.ps1 TREE
#
# Exit: windows-job.sh's; 3 when there is no Git Bash (no verdict).
param([Parameter(Mandatory = $true)][string]$Tree)

$git = Get-Command git.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
$roots = @('C:/Program Files/Git')
if ($git) { $roots = @(Split-Path (Split-Path $git.Source)) + $roots }
$bash = $roots | ForEach-Object { "$_/bin/bash.exe"; "$_/usr/bin/bash.exe" } |
    Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $bash) { Write-Output 'windows-job: no Git Bash on this host (WSL''s bash is not the Windows test host)'; exit 3 }
$env:PATH = (Join-Path $env:USERPROFILE '.cargo/bin') + ';' + (Split-Path $bash) + ';' +
    (Join-Path (Split-Path (Split-Path $bash)) 'usr/bin') + ';' + $env:PATH
& $bash --noprofile --norc 'scripts/lib/windows-job.sh' $Tree
exit $LASTEXITCODE
