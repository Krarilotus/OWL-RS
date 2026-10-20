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
param(
    [Parameter(Mandatory = $true)][int]$ProcessId,
    [Parameter(Mandatory = $true)][double]$LimitGB,
    [string]$Registry = ""
)

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class NreseJob {
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
    const uint ProcessSetQuota = 0x0100, ProcessTerminate = 0x0001, ProcessDupHandle = 0x0040;
    const uint DuplicateSameAccess = 0x2;

    // Returns "" on success, else what failed. bytes 0: no limit.
    public static string Cap(int pid, ulong bytes, string name) {
        IntPtr job = CreateJobObject(IntPtr.Zero, name);
        if (job == IntPtr.Zero) return "CreateJobObject: " + Marshal.GetLastWin32Error();
        if (bytes > 0) {
            var info = new ExtendedLimits();
            info.Basic.LimitFlags = LimitJobMemory;
            info.JobMemoryLimit = new UIntPtr(bytes);
            if (!SetInformationJobObject(job, ExtendedLimitInformation, ref info, (uint)Marshal.SizeOf(typeof(ExtendedLimits))))
                return "SetInformationJobObject: " + Marshal.GetLastWin32Error();
        }
        IntPtr process = OpenProcess(ProcessSetQuota | ProcessTerminate | ProcessDupHandle, false, pid);
        if (process == IntPtr.Zero) return "OpenProcess: " + Marshal.GetLastWin32Error();
        bool ok = AssignProcessToJobObject(job, process);
        int error = Marshal.GetLastWin32Error();
        // A job keeps its name only while a handle to it is open: the process itself gets one,
        // so the name lasts exactly as long as the process (the guard finds the job by it).
        IntPtr theirs;
        if (ok && name != null && !DuplicateHandle(GetCurrentProcess(), job, process, out theirs, 0, false, DuplicateSameAccess)) {
            error = Marshal.GetLastWin32Error();
            CloseHandle(process);
            return "DuplicateHandle: " + error;
        }
        CloseHandle(process);
        // This script's own handle closes when it ends; the job lives on while the process is in it.
        return ok ? "" : "AssignProcessToJobObject: " + error;
    }
}
"@

$bytes = [uint64]($LimitGB * 1GB)
$created = (Get-CimInstance Win32_Process -Filter "ProcessId=$ProcessId").CreationDate
$name = "nrese-build-$ProcessId-$(if ($created) { $created.ToFileTimeUtc() } else { 0 })"
$failed = [NreseJob]::Cap($ProcessId, $bytes, $name)
if ($failed) {
    [Console]::Error.WriteLine("memory-cap: $failed (process $ProcessId left uncapped)")
    exit 1
}
if ($Registry) {
    New-Item -ItemType Directory -Force $Registry | Out-Null
    Set-Content -Path (Join-Path $Registry $name) -Value $ProcessId
}
$name
