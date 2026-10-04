# Puts a running process into a Windows job object with a memory limit: the process and
# every process it starts afterwards (rustc, test binaries) together may commit at most
# LimitGB; at the limit, an allocation fails and the process ends. Used by
# scripts/cargo-guarded.sh, so that a runaway test can't take the machine (a test
# committed 128 GB on a 64 GB PC on 3 October 2026 and froze it).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/lib/memory-cap.ps1 -ProcessId 1234 -LimitGB 32
#
# The job outlives this script: a job lasts while a process is in it.
param(
    [Parameter(Mandatory = $true)][int]$ProcessId,
    [Parameter(Mandatory = $true)][double]$LimitGB
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

    const int ExtendedLimitInformation = 9;
    const uint LimitJobMemory = 0x200;
    const uint ProcessSetQuota = 0x0100, ProcessTerminate = 0x0001;

    // Returns "" on success, else what failed.
    public static string Cap(int pid, ulong bytes) {
        IntPtr job = CreateJobObject(IntPtr.Zero, null);
        if (job == IntPtr.Zero) return "CreateJobObject: " + Marshal.GetLastWin32Error();
        var info = new ExtendedLimits();
        info.Basic.LimitFlags = LimitJobMemory;
        info.JobMemoryLimit = new UIntPtr(bytes);
        if (!SetInformationJobObject(job, ExtendedLimitInformation, ref info, (uint)Marshal.SizeOf(typeof(ExtendedLimits))))
            return "SetInformationJobObject: " + Marshal.GetLastWin32Error();
        IntPtr process = OpenProcess(ProcessSetQuota | ProcessTerminate, false, pid);
        if (process == IntPtr.Zero) return "OpenProcess: " + Marshal.GetLastWin32Error();
        bool ok = AssignProcessToJobObject(job, process);
        int error = Marshal.GetLastWin32Error();
        CloseHandle(process);
        // The job handle is left open until this script ends; the job then lives on
        // while the process is in it.
        return ok ? "" : "AssignProcessToJobObject: " + error;
    }
}
"@

$bytes = [uint64]($LimitGB * 1GB)
$failed = [NreseJob]::Cap($ProcessId, $bytes)
if ($failed) {
    [Console]::Error.WriteLine("memory-cap: $failed (process $ProcessId left uncapped)")
    exit 1
}
