# Native fleet workload boundary. Reuses memory-cap.ps1's named aggregate JobObject;
# this wrapper owns its lifetime/deadline, not fleet's capacity/presence policy.
param([Parameter(Mandatory=$true)][string]$ArgumentsJson)
$ErrorActionPreference = 'Stop'
# Windows PowerShell emits the JSON array as one pipeline object. Assign it
# directly: @(... ) would wrap it again and join the whole argv as one string.
$argv = ConvertFrom-Json $ArgumentsJson
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
Add-Type -ReferencedAssemblies System.Management.dll,System.Web.Extensions.dll -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Management;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Web.Script.Serialization;
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
  [DllImport("kernel32.dll", SetLastError=true)]
  static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
  [DllImport("kernel32.dll", SetLastError=true)]
  static extern bool IsProcessInJob(IntPtr process, IntPtr job, out bool member);
  [DllImport("kernel32.dll", SetLastError=true)]
  static extern bool GetProcessTimes(IntPtr process, out long created, out long exited, out long kernel, out long user);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
  static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder image, ref uint length);
  [DllImport("kernel32.dll", SetLastError=true)]
  static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
  public sealed class Worker {
    public uint pid;
    public long? creation_filetime;
    public string state, image, command_line, metadata_status, parent_status;
    public uint? parent_pid;
    public int? win32_error;
    public bool command_truncated;
  }
  public sealed class WorkerSnapshot {
    public string began_utc = DateTime.UtcNow.ToString("o"), status = "partial", metadata_status = "not_queried";
    public long elapsed_ms;
    public long? root_creation_filetime;
    public uint? active, assigned, listed;
    public int? win32_error;
    public bool list_complete;
    public List<uint> member_pids = new List<uint>();
    public List<Worker> members = new List<Worker>();
  }
  public sealed class Receipt {
    public string job, step, host, request, source_head, named_job;
    public int root_pid, root_exit, total_budget_ms, capture_budget_ms;
    public bool root_exited;
    public long began_monotonic_ms, deadline_monotonic_ms, capture_deadline_monotonic_ms;
    public WorkerSnapshot snapshot;
    public string capture_error;
  }
  // Called only after the existing final lifecycle Mark/teardown. The captured DTO
  // is no longer mutated. This reader owns storage only, never job/process handles.
  // A unique create-only receipt cannot replace an earlier complete attempt record.
  public static void Publish(Receipt receipt, string path, Stopwatch clock) {
    long deadline = receipt.deadline_monotonic_ms;
    if (clock.ElapsedMilliseconds >= deadline) return;
    int canceled = 0;
    var writer = new Thread(delegate() {
      try {
        if (clock.ElapsedMilliseconds >= deadline) return;
        var serializer = new JavaScriptSerializer { MaxJsonLength = 1048576, RecursionLimit = 16 };
        string json = serializer.Serialize(receipt) + "\n";
        if (Interlocked.CompareExchange(ref canceled, 0, 0) != 0 || clock.ElapsedMilliseconds >= deadline) return;
        // No overwrites, including after a caller timeout. A .partial file is not
        // a complete receipt; filesystem operations themselves are not preemptible.
        string partial = path + ".partial";
        using (var stream = new System.IO.FileStream(partial, System.IO.FileMode.CreateNew,
          System.IO.FileAccess.Write, System.IO.FileShare.Read)) {
          byte[] bytes = new UTF8Encoding(false).GetBytes(json);
          stream.Write(bytes, 0, bytes.Length);
        }
        if (Interlocked.CompareExchange(ref canceled, 0, 0) == 0 && clock.ElapsedMilliseconds < deadline)
          System.IO.File.Move(partial, path);
      } catch (Exception) { /* Evidence failure cannot change completed cleanup/refusal. */ }
    });
    writer.IsBackground = true;
    writer.Start();
    int left = (int)Math.Max(0, deadline - clock.ElapsedMilliseconds);
    if (left <= 0 || !writer.Join(left)) Interlocked.Exchange(ref canceled, 1);
  }
  sealed class Metadata {
    public long created;
    public uint parent;
    public string command;
    public bool truncated;
  }
  static string Clip(string text, int limit) {
    return text == null || text.Length <= limit ? text : text.Substring(0, limit);
  }
  static bool LiveMember(IntPtr process, IntPtr job, Worker row) {
    bool member;
    long created, exited, kernel, user;
    if (!IsProcessInJob(process, job, out member)) {
      row.state = "membership_query_error"; row.win32_error = Marshal.GetLastWin32Error(); return false;
    }
    if (!member) { row.state = "raced_not_member"; return false; }
    if (!GetProcessTimes(process, out created, out exited, out kernel, out user)) {
      row.state = "creation_query_error"; row.win32_error = Marshal.GetLastWin32Error(); return false;
    }
    if (row.creation_filetime.HasValue && row.creation_filetime.Value != created) {
      row.state = "raced_creation"; return false;
    }
    row.creation_filetime = created;
    uint wait = WaitForSingleObject(process, 0);
    if (wait == 0) { row.state = "exited"; return false; }
    if (wait != 258) { row.state = "state_query_error"; row.win32_error = Marshal.GetLastWin32Error(); return false; }
    row.state = "verified_live_member";
    return true;
  }
  // Only documented JobObject/process queries. No parent traversal or process scan.
  // WMI is optional metadata: its background reader owns no native handles and never
  // publishes. A stalled provider cannot hold up cleanup or later alter this receipt.
  public static WorkerSnapshot Remaining(IntPtr job, IntPtr root, Stopwatch clock, long deadline) {
    long began = clock.ElapsedMilliseconds;
    var result = new WorkerSnapshot();
    var handles = new List<KeyValuePair<Worker,IntPtr>>();
    try {
      if (clock.ElapsedMilliseconds >= deadline) { result.status = "budget_exhausted"; return result; }
      long created, exited, kernel, user;
      if (GetProcessTimes(root, out created, out exited, out kernel, out user)) result.root_creation_filetime = created;
      result.active = Active(job);
      // JOBOBJECT_BASIC_PROCESS_ID_LIST: two DWORDs then ULONG_PTR IDs.
      // Three bounded reads, 64/128/256 IDs. ERROR_MORE_DATA retains a partial list.
      for (int capacity = 64; capacity <= 256 && clock.ElapsedMilliseconds < deadline; capacity *= 2) {
        int size = 8 + capacity * IntPtr.Size;
        IntPtr buffer = Marshal.AllocHGlobal(size);
        try {
          Marshal.Copy(new byte[size], 0, buffer, size);
          bool ok = QueryInformationJobObject(job, 3, buffer, (uint)size, IntPtr.Zero);
          int error = ok ? 0 : Marshal.GetLastWin32Error();
          if (!ok && error != 234) {
            result.status = "member_list_query_error"; result.win32_error = error; return result;
          }
          uint assigned = unchecked((uint)Marshal.ReadInt32(buffer, 0));
          uint listed = unchecked((uint)Marshal.ReadInt32(buffer, 4));
          result.assigned = assigned; result.listed = listed;
          result.win32_error = ok ? (int?)null : error;
          result.member_pids.Clear();
          for (int i = 0; i < Math.Min(listed, (uint)capacity); i++) {
            ulong pid = unchecked((ulong)Marshal.ReadIntPtr(buffer, 8 + i * IntPtr.Size).ToInt64());
            if (pid > 0 && pid <= uint.MaxValue && !result.member_pids.Contains((uint)pid)) result.member_pids.Add((uint)pid);
          }
          result.list_complete = ok && listed == assigned && listed <= capacity;
          if (result.list_complete) break;
        } finally { Marshal.FreeHGlobal(buffer); }
      }
      // Detail at most64 members; uninspected IDs/counts stay explicitly partial.
      foreach (uint pid in result.member_pids) {
        if (result.members.Count == 64 || clock.ElapsedMilliseconds >= deadline) break;
        var row = new Worker { pid = pid, metadata_status = "not_queried" };
        result.members.Add(row);
        // PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE; no mutation rights.
        IntPtr process = OpenProcess(0x00101000, false, pid);
        if (process == IntPtr.Zero) {
          row.state = "open_query_error"; row.win32_error = Marshal.GetLastWin32Error(); continue;
        }
        handles.Add(new KeyValuePair<Worker,IntPtr>(row, process));
        if (!LiveMember(process, job, row)) continue;
        uint length = 1024;
        var image = new StringBuilder((int)length);
        if (QueryFullProcessImageName(process, 0, image, ref length)) row.image = image.ToString();
        else row.win32_error = Marshal.GetLastWin32Error();
      }
      var expected = new Dictionary<uint,long>();
      foreach (var pair in handles) {
        if (clock.ElapsedMilliseconds >= deadline) break;
        if (LiveMember(pair.Value, job, pair.Key)) expected.Add(pair.Key.pid, pair.Key.creation_filetime.Value);
      }
      int left = (int)Math.Max(0, deadline - clock.ElapsedMilliseconds);
      if (expected.Count == 0 || left <= 0) { result.metadata_status = "no_live_members_or_budget"; return result; }
      var metadata = new Dictionary<uint,Metadata>();
      string metadataError = null;
      int queryBudget = left;
      var reader = new Thread(delegate() {
        try {
          if (clock.ElapsedMilliseconds >= deadline) return;
          var ids = new List<string>();
          foreach (uint pid in expected.Keys) ids.Add("ProcessId=" + pid.ToString(System.Globalization.CultureInfo.InvariantCulture));
          var options = new EnumerationOptions { ReturnImmediately = true, Rewindable = false, BlockSize = 1,
            Timeout = TimeSpan.FromMilliseconds(queryBudget) };
          var scope = new ManagementScope(@"\\.\root\cimv2");
          scope.Options.Timeout = TimeSpan.FromMilliseconds(queryBudget);
          if (clock.ElapsedMilliseconds >= deadline) return;
          using (var query = new ManagementObjectSearcher(scope, new ObjectQuery(
            "SELECT ProcessId,ParentProcessId,CreationDate,CommandLine FROM Win32_Process WHERE " + string.Join(" OR ", ids.ToArray())), options))
          using (var rows = query.Get()) {
            foreach (ManagementObject row in rows) using (row) {
              if (clock.ElapsedMilliseconds >= deadline || metadata.Count >= expected.Count) break;
              uint pid = Convert.ToUInt32(row["ProcessId"]);
              long identity;
              if (!expected.TryGetValue(pid, out identity)) continue;
              long made = ManagementDateTimeConverter.ToDateTime((string)row["CreationDate"]).ToUniversalTime().ToFileTimeUtc();
              // WMI datetime has microsecond resolution; the retained handle has100ns.
              // Main thread also rechecks that this exact handle stayed live/in-job.
              if (made / 10 != identity / 10) continue;
              string command = row["CommandLine"] as string;
              metadata[pid] = new Metadata { created = made, parent = Convert.ToUInt32(row["ParentProcessId"]),
                command = Clip(command, 2048), truncated = command != null && command.Length > 2048 };
            }
          }
        } catch (Exception error) { metadataError = error.GetType().Name + ":" + error.HResult; }
      });
      reader.IsBackground = true;
      reader.Start();
      left = (int)Math.Max(0, deadline - clock.ElapsedMilliseconds);
      // Never abort/kill a metadata thread, and never read its mutable data on timeout.
      if (left <= 0 || !reader.Join(left)) { result.metadata_status = "timeout"; return result; }
      result.metadata_status = metadataError == null ? "returned" : metadataError;
      foreach (var pair in handles) {
        var row = pair.Key;
        if (clock.ElapsedMilliseconds >= deadline) { row.metadata_status = "budget_exhausted_before_recheck"; continue; }
        if (!LiveMember(pair.Value, job, row)) { row.metadata_status = "raced_or_exited"; continue; }
        Metadata found;
        if (!metadata.TryGetValue(row.pid, out found)) { row.metadata_status = "missing_or_creation_unverified"; continue; }
        row.command_line = found.command; row.command_truncated = found.truncated;
        row.parent_pid = found.parent;
        row.metadata_status = "creation_microsecond_match_and_live_handle_rechecked";
        row.parent_status = "unresolved_parent_pid";
        foreach (var candidate in handles) {
          if (clock.ElapsedMilliseconds >= deadline) break;
          if (candidate.Key.pid == found.parent && candidate.Key.state == "verified_live_member" &&
              candidate.Key.creation_filetime <= row.creation_filetime)
            row.parent_status = "same_snapshot_member_creation_precedes_child";
        }
      }
      result.status = result.list_complete && result.members.Count == result.member_pids.Count ? "captured" : "partial";
      return result;
    } finally {
      foreach (var pair in handles) CloseHandle(pair.Value);
      result.elapsed_ms = clock.ElapsedMilliseconds - began;
    }
  }
}
'@
$psi = New-Object Diagnostics.ProcessStartInfo
# bin/bash.exe is a launcher: its pre-existing real Bash child would escape
# subsequent JobObject admission. Start the real process whose PID is capped.
. (Join-Path $PSScriptRoot 'git-bash.ps1')
$psi.FileName = Resolve-GitBash
# This real Bash is not a login launcher. Its early cygpath/sleep/tee calls
# need the selected installation on the child PATH, before the cap handshake.
$psi.EnvironmentVariables['PATH'] = [IO.Path]::GetDirectoryName($psi.FileName) + ';' + $env:PATH
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
$diagnosticUntil = 0
$diagnosticReceipt = $null
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
        # Capture only: no serializer, artifact write or diagnostic Mark may delay
        # forced cleanup or consume storage needed by its existing lifecycle log.
        # Capture and later publication share one total1s/unit/stop budget.
        $diagnosticBegan = $clock.ElapsedMilliseconds
        $diagnosticUntil = [Math]::Min($diagnosticBegan + 1000,
            [Math]::Min($seconds * 1000, $jobStopUntil))
        try {
            $captureUntil = [Math]::Min($diagnosticBegan + 500, $diagnosticUntil)
            $budget = [int][Math]::Max(0, $captureUntil - $clock.ElapsedMilliseconds)
            $snapshot = [GateJobLifetime]::Remaining($handle, $p.Handle, $clock, $captureUntil)
            $diagnosticReceipt = [GateJobLifetime+Receipt]@{ job=$env:FLEET_JOB; step=$env:FLEET_STEP; host=$env:FLEET_HOST;
                request=$env:NRESE_GATE_REQUEST; source_head=$env:NRESE_GATE_HEAD;
                named_job=$jobName; root_pid=$p.Id; root_exited=$p.HasExited; root_exit=$p.ExitCode;
                began_monotonic_ms=$diagnosticBegan; deadline_monotonic_ms=$diagnosticUntil;
                capture_deadline_monotonic_ms=$captureUntil;
                total_budget_ms=[Math]::Max(0,$diagnosticUntil-$diagnosticBegan);
                capture_budget_ms=$budget; snapshot=$snapshot }
        } catch {
            # No diagnostic I/O, logging or errors may escape into cleanup.
            try { $diagnosticReceipt = [GateJobLifetime+Receipt]@{
                job=$env:FLEET_JOB; step=$env:FLEET_STEP; named_job=$jobName; root_pid=$p.Id;
                began_monotonic_ms=$diagnosticBegan; deadline_monotonic_ms=$diagnosticUntil;
                capture_error=$_.Exception.GetType().Name } } catch { }
        }
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
# Final lifecycle Mark, cap removal and native handle teardown are already complete.
# Only the remaining original diagnostic/unit/stop budget is available; no added wait.
if ($null -ne $diagnosticReceipt) {
    try {
        $left = [int][Math]::Max(0, $diagnosticUntil - $clock.ElapsedMilliseconds)
        if ($left -gt 0) {
            $path = Join-Path $env:FLEET_OUT "unit-$n.remaining-workers-$PID.json"
            [GateJobLifetime]::Publish($diagnosticReceipt, $path, $clock)
        }
    } catch { }
}
exit $code
