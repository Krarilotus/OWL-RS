# Puts a running process into a Windows job object with a memory limit: the process and
# every process it starts afterwards (rustc, test binaries) together may commit at most
# LimitGB; at the limit, an allocation fails and the process ends. Used by
# scripts/cargo-guarded.sh, so that a runaway test can't take the machine (a test
# committed 128 GB on a 64 GB PC on 3 October 2026 and froze it).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/lib/memory-cap.ps1 -ProcessId 1234 -LimitGB 32 [-Registry DIR]
#
# The job outlives this script: a job lasts while a process is in it. LimitGB 0: no limit.
#
# The job is named after the process's identity, nrese-build-<pid>-<creation time>, and the name
# printed. With -Registry it is also listed there (a file of that name), for the main PC's guard:
# it asks each listed job whether a process it stops is in it, and names those jobs in its notice,
# so the build that started the process learns it was stopped (scripts/cargo-guarded.sh). A job's
# processes stay in it whatever becomes of their parents (Git Bash's exec leaves none).
#
# The work is a small program (below), compiled once into memory-cap-<hash>.exe in the temporary
# directory's nrese-memory-cap, <hash> the first 12 hex digits of this file's SHA-256. This script
# builds it if it isn't there and runs it; scripts/cargo-guarded.sh runs it directly when it is
# (PowerShell's start and a compile took seconds at every build: Sol, 20 Oct 2026).
#
#   memory-cap-<hash>.exe PID LIMIT_GB [REGISTRY]
param(
    [Parameter(Mandatory = $true)][int]$ProcessId,
    [Parameter(Mandatory = $true)][double]$LimitGB,
    [string]$Registry = "",
    # Build the program again (the cached one didn't run).
    [switch]$Rebuild
)

$source = @"
using System;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;

public static class NreseMemoryCap {
    [StructLayout(LayoutKind.Sequential)]
    struct BasicLimits {
        public long PerProcessUserTimeLimit;
        public long PerJobUserTimeLimit;
        public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize;
        public UIntPtr MaximumWorkingSetSize;
        public uint ActiveProcessLimit;
        public UIntPtr Affinity;
        public uint PriorityClass;
        public uint SchedulingClass;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct IoCounters {
        public ulong ReadOperationCount, WriteOperationCount, OtherOperationCount;
        public ulong ReadTransferCount, WriteTransferCount, OtherTransferCount;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct ExtendedLimits {
        public BasicLimits Basic;
        public IoCounters Io;
        public UIntPtr ProcessMemoryLimit;
        public UIntPtr JobMemoryLimit;
        public UIntPtr PeakProcessMemoryUsed;
        public UIntPtr PeakJobMemoryUsed;
    }

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern IntPtr CreateJobObject(IntPtr attributes, string name);

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetInformationJobObject(IntPtr job, int infoClass, ref ExtendedLimits info, uint length);

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr OpenProcess(uint access, bool inherit, int pid);

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetProcessTimes(IntPtr process, out long created, out long exited, out long kernel, out long user);

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DuplicateHandle(IntPtr sourceProcess, IntPtr source, IntPtr targetProcess,
        out IntPtr target, uint access, bool inherit, uint options);

    [DllImport("kernel32.dll")]
    static extern IntPtr GetCurrentProcess();

    const int ExtendedLimitInformation = 9;
    const uint LimitJobMemory = 0x200;
    const uint ProcessSetQuota = 0x0100, ProcessTerminate = 0x0001, ProcessDupHandle = 0x0040,
        ProcessQueryLimited = 0x1000;
    const uint DuplicateSameAccess = 0x2;

    static int Fail(string what, int pid) {
        Console.Error.WriteLine("memory-cap: " + what + ": " + Marshal.GetLastWin32Error() + " (process " + pid + " left uncapped)");
        return 1;
    }

    public static int Main(string[] args) {
        int pid = int.Parse(args[0], CultureInfo.InvariantCulture);
        ulong bytes = (ulong)(double.Parse(args[1], CultureInfo.InvariantCulture) * (1UL << 30));
        string registry = args.Length > 2 ? args[2] : "";
        IntPtr process = OpenProcess(ProcessSetQuota | ProcessTerminate | ProcessDupHandle | ProcessQueryLimited, false, pid);
        if (process == IntPtr.Zero) return Fail("OpenProcess", pid);
        long created, exited, kernel, user;
        if (!GetProcessTimes(process, out created, out exited, out kernel, out user)) return Fail("GetProcessTimes", pid);
        string name = "nrese-build-" + pid + "-" + created;
        IntPtr job = CreateJobObject(IntPtr.Zero, name);
        if (job == IntPtr.Zero) return Fail("CreateJobObject", pid);
        if (bytes > 0) {
            var info = new ExtendedLimits();
            info.Basic.LimitFlags = LimitJobMemory;
            info.JobMemoryLimit = new UIntPtr(bytes);
            if (!SetInformationJobObject(job, ExtendedLimitInformation, ref info, (uint)Marshal.SizeOf(typeof(ExtendedLimits))))
                return Fail("SetInformationJobObject", pid);
        }
        if (!AssignProcessToJobObject(job, process)) return Fail("AssignProcessToJobObject", pid);
        // A job keeps its name only while a handle to it is open: the process itself gets one,
        // so the name lasts exactly as long as the process (the guard finds the job by it).
        IntPtr theirs;
        if (!DuplicateHandle(GetCurrentProcess(), job, process, out theirs, 0, false, DuplicateSameAccess))
            return Fail("DuplicateHandle", pid);
        CloseHandle(process);
        if (registry != "") {
            Directory.CreateDirectory(registry);
            File.WriteAllText(Path.Combine(registry, name), pid.ToString(CultureInfo.InvariantCulture));
        }
        Console.WriteLine(name);
        // This program's own handle closes when it ends; the job lives on while the process is in it.
        return 0;
    }
}
"@

$hash = (Get-FileHash -Algorithm SHA256 -Path $PSCommandPath).Hash.Substring(0, 12).ToLowerInvariant()
$cache = Join-Path ([IO.Path]::GetTempPath()) 'nrese-memory-cap'
$exe = Join-Path $cache "memory-cap-$hash.exe"
# A fresh build under a name of its own, then put in place (over a cached one that doesn't run);
# if that one can't be replaced (another build runs it), the fresh one is used where it is.
function Build {
    New-Item -ItemType Directory -Force $cache | Out-Null
    $part = "$cache\memory-cap-$hash.$PID.tmp.exe"
    Add-Type -TypeDefinition $source -OutputAssembly $part -OutputType ConsoleApplication
    try {
        Move-Item -Path $part -Destination $exe -Force -ErrorAction Stop
        return $exe
    } catch {
        return $part
    }
}
function Run($program) {
    try { & $program @arguments | ForEach-Object { [Console]::Out.WriteLine($_) }; return $LASTEXITCODE } catch { [Console]::Error.WriteLine("memory-cap: $program doesn't run: $_"); return 1 }
}
$arguments = @("$ProcessId", $LimitGB.ToString([Globalization.CultureInfo]::InvariantCulture))
if ($Registry) { $arguments += $Registry }
$built = $Rebuild -or -not (Test-Path $exe)
$program = if ($built) { Build } else { $exe }
$code = Run $program
# A cached program that doesn't run is built again, once.
if ($code -ne 0 -and -not $built) {
    $program = Build
    $code = Run $program
}
if ($program -ne $exe) { Remove-Item -Path $program -ErrorAction SilentlyContinue }
exit $code
