# Use the host's configured PATH, as fleet readiness does. Resolve Git for Windows'
# real Bash, never its bin/bash.exe launcher or the unrelated Windows/WSL alias.
function Resolve-GitBash {
    $git = Get-Command git.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $git) { throw 'native gate requires Git for Windows: git.exe was not found on PATH' }
    $directory = Split-Path -Parent $git.Source
    $leaf = Split-Path -Leaf $directory
    $root = Split-Path -Parent $directory
    if ($leaf -eq 'git-core' -and (Split-Path -Leaf $root) -eq 'libexec' -and
        (Split-Path -Leaf (Split-Path -Parent $root)) -in @('mingw64','mingw32')) {
        # Git prepends its exec-path inside hooks. Stay in that selected install.
        $root = Split-Path -Parent (Split-Path -Parent $root)
    } elseif ($leaf -eq 'bin') {
        if ((Split-Path -Leaf $root) -in @('mingw64','mingw32','usr')) {
            $root = Split-Path -Parent $root
        }
    } elseif ($leaf -ne 'cmd') {
        throw "native gate cannot resolve Git for Windows root from selected git.exe: $($git.Source)"
    }
    $bash = Join-Path $root 'usr/bin/bash.exe'
    if (-not (Test-Path -LiteralPath $bash -PathType Leaf)) {
        throw "native gate requires real Git Bash at '$bash' (selected git.exe: '$($git.Source)'); configure PATH to the installed Git for Windows"
    }
    return [IO.Path]::GetFullPath($bash)
}
