# Native fleet workload boundary. Reuses memory-cap.ps1's named aggregate JobObject;
# this wrapper owns its lifetime/deadline, not fleet's capacity/presence policy.
param([Parameter(Mandatory=$true)][string]$ArgumentsJson)
$ErrorActionPreference = 'Stop'
$argv = @(ConvertFrom-Json $ArgumentsJson)
$n = [int]$env:FLEET_STEP - 1
if (-not $env:FLEET_JOB -or $n -lt 0) { throw 'not an official fleet step' }
$need = @(ConvertFrom-Json $env:NRESE_GATE_NEEDS)[$n]
$seconds = [int]$env:NRESE_GATE_UNIT_S
if ($seconds -lt 1 -or $seconds -gt 3300 -or [double]$need.ram_gb -le 0) { throw 'invalid workload envelope' }
$capFile = Join-Path $env:FLEET_OUT "unit-$n.cap"
Remove-Item -LiteralPath $capFile -ErrorAction SilentlyContinue
$env:NRESE_GATE_CAP_FILE = $capFile
$env:NRESE_GATE_BOUNDED = '1'
# Every owned native child needs a real EOF input, including setup and cleanup.
$phaseLog = Join-Path $env:FLEET_OUT "unit-$n.lifecycle.log"
function Mark([string]$phase) {
    [IO.File]::AppendAllText($phaseLog, ([DateTime]::UtcNow.ToString('o')+' '+$phase+"`n"))
}
function StopOwnedTree([Diagnostics.Process]$ownedProcess) {
    if (-not $ownedProcess -or $ownedProcess.HasExited) { return $true }
    try {
        $stopLog = Join-Path $env:FLEET_OUT "unit-$n.stop-$($ownedProcess.Id).log"
        $stopErr = $stopLog+'.err'
        $stopInfo = New-Object Diagnostics.ProcessStartInfo
        $stopInfo.FileName = Join-Path $env:SystemRoot 'System32/taskkill.exe'
        $stopInfo.Arguments = '/PID '+$ownedProcess.Id+' /T /F'
        $stopInfo.UseShellExecute = $false; $stopInfo.CreateNoWindow = $true
        $stopInfo.RedirectStandardInput = $true
        $stopInfo.RedirectStandardOutput = $true; $stopInfo.RedirectStandardError = $true
        $stopper = New-Object Diagnostics.Process; $stopper.StartInfo = $stopInfo
        if (-not $stopper.Start()) { return $false }
        $stopper.StandardInput.Close()
        $stopOutput = $stopper.StandardOutput.ReadToEndAsync()
        $stopError = $stopper.StandardError.ReadToEndAsync()
        if (-not $stopper.WaitForExit(10000)) { $stopper.Kill(); return $false }
        if (-not $stopOutput.Wait(1000) -or -not $stopError.Wait(1000)) { return $false }
        [IO.File]::WriteAllText($stopLog, $stopOutput.Result)
        [IO.File]::WriteAllText($stopErr, $stopError.Result)
        if (-not $ownedProcess.WaitForExit(2000)) { return $false }
        return $stopper.ExitCode -eq 0
    } catch { Write-Error "owned native tree stop failed: $_" -ErrorAction Continue; return $false }
}
# The CRT argument convention, including embedded quotes and trailing backslashes.
function QuoteArg([string]$value) {
    '"' + [regex]::Replace([regex]::Replace($value, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1') + '"'
}
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class GateJobLifetime {
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
  public static extern IntPtr OpenJobObject(uint access, bool inherit, string name);
  [DllImport("kernel32.dll", SetLastError=true)]
  public static extern bool TerminateJobObject(IntPtr job, uint code);
  [DllImport("kernel32.dll")]
  public static extern bool CloseHandle(IntPtr handle);
  [DllImport("kernel32.dll", SetLastError=true)]
  static extern bool QueryInformationJobObject(IntPtr job, int type, IntPtr info, uint size, IntPtr returned);
  public static uint Active(IntPtr job) {
    // JOBOBJECT_BASIC_ACCOUNTING_INFORMATION: four LARGE_INTEGERs, then four DWORDs.
    IntPtr info = Marshal.AllocHGlobal(48);
    try {
      if (!QueryInformationJobObject(job, 1, info, 48, IntPtr.Zero)) throw new InvalidOperationException("job accounting unavailable");
      return unchecked((uint)Marshal.ReadInt32(info, 40));
    } finally { Marshal.FreeHGlobal(info); }
  }
}
'@
$psi = New-Object Diagnostics.ProcessStartInfo
# bin/bash.exe is a launcher: its pre-existing real Bash child would escape
# subsequent JobObject admission. Start the real process whose PID is capped.
. (Join-Path $PSScriptRoot 'git-bash.ps1')
$psi.FileName = Resolve-GitBash
$psi.Arguments = (($argv | ForEach-Object { QuoteArg ([string]$_) }) -join ' ')
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
# Fleet's detached entry may have no native stdin handle. Give Bash and its native
# Python/Cargo descendants an actual EOF pipe rather than inheriting an invalid
# GetStdHandle(STD_INPUT_HANDLE). This is input plumbing, not a worker policy.
$psi.RedirectStandardInput = $true
$p = New-Object Diagnostics.Process
$p.StartInfo = $psi
$handle = [IntPtr]::Zero
$helper = $null
$code = 75
$clock = [Diagnostics.Stopwatch]::StartNew()
$jobStopUntil = 0
try {
    if (-not $p.Start()) { throw 'native Git Bash did not start' }
    $p.StandardInput.Close()
    Mark "bash-started $($p.Id)"
    # Bash waits for capFile before starting any gate/setup workers. Its own PID is the
    # helper's target; every later child inherits the same aggregate memory envelope.
    $helperOut = Join-Path $env:FLEET_OUT "unit-$n.cap-helper.log"
    $helperErr = Join-Path $env:FLEET_OUT "unit-$n.cap-helper.err"
    $helperArgs = @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File',
        'scripts/lib/memory-cap.ps1', '-ProcessId', [string]$p.Id, '-LimitGB',
        ([double]$need.ram_gb).ToString([Globalization.CultureInfo]::InvariantCulture))
    # Own the actual process handle and output tasks. Windows PowerShell's
    # Start-Process passthrough may have no ExitCode and asynchronous files may
    # still be empty after WaitForExit(int), so neither is a trustworthy verdict.
    Mark "helper-starting"
    [IO.File]::WriteAllText($helperOut, '')
    [IO.File]::WriteAllText($helperErr, '')
    $helperInfo = New-Object Diagnostics.ProcessStartInfo
    $helperInfo.FileName = 'powershell.exe'
    $helperInfo.Arguments = (($helperArgs | ForEach-Object { QuoteArg $_ }) -join ' ')
    $helperInfo.UseShellExecute = $false; $helperInfo.CreateNoWindow = $true
    $helperInfo.RedirectStandardInput = $true
    $helperInfo.RedirectStandardOutput = $true; $helperInfo.RedirectStandardError = $true
    $helper = New-Object Diagnostics.Process; $helper.StartInfo = $helperInfo
    if (-not $helper.Start()) { throw 'memory helper did not start' }
    $helper.StandardInput.Close()
    $helperOutput = $helper.StandardOutput.ReadToEndAsync()
    $helperError = $helper.StandardError.ReadToEndAsync()
    Mark "helper-started $($helper.Id)"
    $setupMs = [Math]::Max(1, [Math]::Min(60000, ($seconds * 1000) - [int]$clock.ElapsedMilliseconds))
    $setupUntil = $clock.ElapsedMilliseconds + $setupMs
    if (-not $helper.WaitForExit($setupMs)) { throw 'memory helper setup deadline' }
    foreach ($read in @($helperOutput,$helperError)) {
        $left = [Math]::Max(1, $setupUntil - $clock.ElapsedMilliseconds)
        if (-not $read.Wait([int]$left)) { throw 'memory helper output deadline' }
    }
    [IO.File]::WriteAllText($helperOut, $helperOutput.Result)
    [IO.File]::WriteAllText($helperErr, $helperError.Result)
    Mark "helper-exit $($helper.ExitCode)"
    if ($helper.ExitCode -ne 0) { throw 'cannot attach existing memory helper' }
    $jobName = ([IO.File]::ReadAllText($helperOut)).Trim()
    if (-not $jobName) { throw 'memory helper returned no job identity' }
    # JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE. Retain the handle even if Bash exits.
    $handle = [GateJobLifetime]::OpenJobObject(0x000c, $false, $jobName)
    if ($handle -eq [IntPtr]::Zero) { throw 'cannot own native workload lifetime' }
    [IO.File]::WriteAllText($capFile, $jobName, (New-Object Text.UTF8Encoding($false)))
    Mark "cap-attached $jobName"
    $remaining = [Math]::Max(1, ($seconds * 1000) - [int]$clock.ElapsedMilliseconds)
    if (-not $p.WaitForExit($remaining)) {
        Mark "deadline"
        $jobStopUntil = $clock.ElapsedMilliseconds + 10000
        if (-not [GateJobLifetime]::TerminateJobObject($handle, 75)) { throw 'native deadline could not stop its JobObject' }
        $stopLeft = [int][Math]::Max(1, $jobStopUntil - $clock.ElapsedMilliseconds)
        if (-not $p.WaitForExit($stopLeft)) { throw 'native JobObject termination did not settle within 10 seconds' }
        Write-Error 'native unit deadline: no verdict' -ErrorAction Continue
    } else {
        # Let already-ending log writers close, still within the unit's whole deadline.
        $settleUntil = [Math]::Min($seconds * 1000, $clock.ElapsedMilliseconds + 2000)
        while ([GateJobLifetime]::Active($handle) -ne 0 -and $clock.ElapsedMilliseconds -lt $settleUntil) {
            Start-Sleep -Milliseconds 25
        }
        if ([GateJobLifetime]::Active($handle) -ne 0) {
        # A command cannot report a completed unit while it still owns workers.
        $jobStopUntil = $clock.ElapsedMilliseconds + 10000
        if (-not [GateJobLifetime]::TerminateJobObject($handle, 75)) { throw 'native unit left workers that could not be stopped' }
        Write-Error 'native unit left workers: no verdict' -ErrorAction Continue
        } else { $code = $p.ExitCode }
    }
} finally {
    Mark "cleanup-start"
    if ($helper -and -not $helper.HasExited) {
        if (-not (StopOwnedTree $helper)) { $code = 75; Write-Error 'memory helper tree did not stop' -ErrorAction Continue }
    }
    if ($handle -ne [IntPtr]::Zero) {
        if ([GateJobLifetime]::Active($handle) -ne 0) {
            if ($jobStopUntil -eq 0) { $jobStopUntil = $clock.ElapsedMilliseconds + 10000 }
            if (-not [GateJobLifetime]::TerminateJobObject($handle, 75)) {
                $code = 75; Write-Error 'native cleanup could not stop its JobObject' -ErrorAction Continue
            }
            # Process exit alone does not prove its descendants released handles.
            # Share the existing 10-second bound with the process wait above.
            while ([GateJobLifetime]::Active($handle) -ne 0 -and $clock.ElapsedMilliseconds -lt $jobStopUntil) {
                Start-Sleep -Milliseconds 25
            }
        }
        $active = [GateJobLifetime]::Active($handle)
        Mark "job-active-final $active"
        if ($active -ne 0) { $code = 75; Write-Error 'native JobObject did not settle: no verdict' -ErrorAction Continue }
        [void][GateJobLifetime]::CloseHandle($handle)
    } elseif ($p.Id -and -not $p.HasExited) {
        # The pre-cap waiter can own cygpath/sleep children too. Stop its tree.
        if (-not (StopOwnedTree $p)) { $code = 75; Write-Error 'pre-cap native tree did not stop' -ErrorAction Continue }
    }
    Remove-Item -LiteralPath $capFile -ErrorAction SilentlyContinue
    Mark "cleanup-end code=$code"
}
exit $code
