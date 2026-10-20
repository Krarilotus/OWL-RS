#!/usr/bin/env python3
"""What a Linux host's state is when a timing starts: the CPU frequency policy (driver,
governor, energy preference, boost), transparent huge pages, the CPU and cache topology,
the NUMA nodes, the load and who else is busy, memory and swap, temperatures, and what this
user may change without a password. One JSON object on stdout; read-only, never changes
anything (benches/PROTOCOL.md §5, "Host hygiene").

    hygiene.py                 the state now
    hygiene.py --cpus 0-7      ... and whether that CPU list lies on one L3 and NUMA node
    hygiene.py --check         exit 1 when the state isn't what timings need (`problems`)

fast.py records it beside every pass (`host_state`), so a pass that met a different host
(a governor changed, boost toggled, another user busy) is visible in its own report.
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

SYS = Path("/sys/devices/system")


def read(path, default=None):
    try:
        return Path(path).read_text().strip()
    except OSError:
        return default


def cpu_list(text: str) -> list[int]:
    out = []
    for part in (text or "").split(","):
        if "-" in part:
            a, b = part.split("-")
            out += range(int(a), int(b) + 1)
        elif part.strip():
            out.append(int(part))
    return out


def frequency() -> dict:
    policies = sorted(glob.glob(str(SYS / "cpu/cpufreq/policy*")))
    values = {key: sorted({read(f"{p}/{key}") for p in policies} - {None}) for key in
              ("scaling_driver", "scaling_governor", "energy_performance_preference",
               "scaling_min_freq", "scaling_max_freq", "cpuinfo_max_freq")}
    current = [int(v) for p in policies if (v := read(f"{p}/scaling_cur_freq")) and v.isdigit()]
    boost = read(SYS / "cpu/cpufreq/boost")
    if boost is None:  # amd-pstate exposes it per policy
        per = sorted({read(f"{p}/boost") for p in policies} - {None})
        boost = per[0] if len(per) == 1 else (",".join(per) or None)
    return {"driver": values["scaling_driver"], "governor": values["scaling_governor"],
            "epp": values["energy_performance_preference"], "boost": boost,
            "amd_pstate": read(SYS / "cpu/amd_pstate/status"),
            "min_khz": values["scaling_min_freq"], "max_khz": values["scaling_max_freq"],
            "cur_khz": [min(current), max(current)] if current else None}


def topology(cpus: list[int] | None) -> dict:
    online = cpu_list(read(SYS / "cpu/online", ""))
    l3 = sorted({read(f"{SYS}/cpu/cpu{c}/cache/index3/shared_cpu_list") for c in online} - {None})
    nodes = {Path(n).name: read(f"{n}/cpulist") for n in sorted(glob.glob(str(SYS / "node/node[0-9]*")))}
    out = {"online": len(online), "smt": read(SYS / "cpu/smt/active"), "l3_domains": l3, "numa_nodes": nodes,
           "isolated": read(SYS / "cpu/isolated"), "nohz_full": read(SYS / "cpu/nohz_full")}
    if cpus is not None:
        out["cpus"] = cpus
        out["cpus_one_l3"] = any(set(cpus) <= set(cpu_list(d)) for d in l3)
        out["cpus_one_node"] = any(set(cpus) <= set(cpu_list(c)) for c in nodes.values()) if nodes else True
    return out


def busy(seconds: float = 1.0) -> dict:
    """Processes that used more than a tenth of a core over `seconds`, by user."""
    def ticks():
        out = {}
        for stat in glob.glob("/proc/[0-9]*/stat"):
            try:
                text = Path(stat).read_text()
                pid = int(stat.split("/")[2])
                fields = text.rsplit(")", 1)[1].split()
                out[pid] = (int(fields[11]) + int(fields[12]), text[text.index("(") + 1:text.rindex(")")])
            except (OSError, ValueError, IndexError):
                continue
        return out
    hz = os.sysconf("SC_CLK_TCK")
    a = ticks()
    time.sleep(seconds)
    b = ticks()
    me = os.getuid()
    found = []
    for pid, (t, name) in b.items():
        used = (t - a.get(pid, (t, name))[0]) / hz / seconds
        if used > 0.1:
            try:
                uid = os.stat(f"/proc/{pid}").st_uid
            except OSError:
                continue
            if pid != os.getpid():
                found.append({"pid": pid, "name": name, "cores": round(used, 2), "mine": uid == me})
    return {"loadavg": read("/proc/loadavg"), "busy": sorted(found, key=lambda x: -x["cores"])[:10]}


def memory() -> dict:
    info = {}
    for line in (read("/proc/meminfo", "") or "").splitlines():
        key, _, rest = line.partition(":")
        if key in ("MemTotal", "MemAvailable", "Cached", "Dirty", "SwapTotal", "SwapFree", "AnonHugePages"):
            info[key] = int(rest.split()[0]) // 1024
    return {"mib": info,
            "thp": read("/sys/kernel/mm/transparent_hugepage/enabled"),
            "thp_defrag": read("/sys/kernel/mm/transparent_hugepage/defrag"),
            "swappiness": read("/proc/sys/vm/swappiness"),
            "numa_balancing": read("/proc/sys/kernel/numa_balancing"),
            "aslr": read("/proc/sys/kernel/randomize_va_space"),
            # Pressure stall information: fast.py records each timed process's stalls with it.
            "psi": Path("/proc/pressure/cpu").exists()}


def temperatures() -> dict:
    out = {}
    for hw in glob.glob("/sys/class/hwmon/hwmon*"):
        name = read(f"{hw}/name")
        if name in ("k10temp", "coretemp", "zenpower"):
            temps = [int(v) / 1000 for f in glob.glob(f"{hw}/temp*_input") if (v := read(f)) and v.lstrip("-").isdigit()]
            if temps:
                out[name] = max(temps)
    return out


def allowed() -> dict:
    """What this user may run as root without a password (sudo -n -l), and whether the user's
    systemd delegates the cpuset controller (AllowedCPUs= on a user scope)."""
    try:
        sudo = subprocess.run(["sudo", "-n", "-l"], capture_output=True, text=True, timeout=10)
        rules = [line.strip() for line in sudo.stdout.splitlines() if "NOPASSWD" in line]
    except (OSError, subprocess.TimeoutExpired):
        rules = []
    uid = os.getuid()
    controllers = read(f"/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/cgroup.controllers", "")
    return {"sudo_nopasswd": rules, "user_cgroup_controllers": (controllers or "").split(),
            "taskset": bool(subprocess.run(["sh", "-c", "command -v taskset"], capture_output=True).stdout),
            "numactl": bool(subprocess.run(["sh", "-c", "command -v numactl"], capture_output=True).stdout)}


def presence() -> dict:
    try:
        who = subprocess.run(["who"], capture_output=True, text=True, timeout=5).stdout.splitlines()
    except (OSError, subprocess.TimeoutExpired):
        who = []
    indexers = [p for p in ("localsearch-3", "tracker-miner-f", "tracker-miner-fs-3", "baloo_file")
                if subprocess.run(["pgrep", "-x", p], capture_output=True).returncode == 0]
    return {"sessions": len(who), "indexers_running": indexers}


def problems(state: dict) -> list[str]:
    """What makes timings noisier than they need be, each a fix in PROTOCOL.md §5."""
    out = []
    f, m, t = state["frequency"], state["memory"], state["topology"]
    if f["governor"] and f["governor"] != ["performance"]:
        out.append(f"governor {','.join(f['governor'])}, not performance")
    if f["epp"] and f["epp"] != ["performance"]:
        out.append(f"energy preference {','.join(f['epp'])}, not performance")
    if m["thp"] and "[always]" in m["thp"]:
        out.append("transparent huge pages: always (allocation-dependent compaction stalls)")
    if t.get("cpus") is not None and not (t["cpus_one_l3"] and t["cpus_one_node"]):
        out.append(f"the CPUs {t['cpus']} span more than one L3 or NUMA node")
    others = [b for b in state["load"]["busy"] if not b["mine"]]
    if others:
        out.append("other users busy: " + ", ".join(f"{b['name']} {b['cores']}" for b in others[:3]))
    if state["presence"]["indexers_running"]:
        out.append("a desktop file indexer runs: " + ", ".join(state["presence"]["indexers_running"]))
    if m["mib"].get("SwapTotal", 0) - m["mib"].get("SwapFree", 0) > 256:
        out.append(f"{m['mib']['SwapTotal'] - m['mib']['SwapFree']} MiB swapped out")
    return out


def state(cpus: list[int] | None = None, sample_s: float = 1.0) -> dict:
    s = {"host": os.uname().nodename, "kernel": os.uname().release, "at": time.strftime("%Y-%m-%dT%H:%M:%S"),
         "frequency": frequency(), "topology": topology(cpus), "load": busy(sample_s), "memory": memory(),
         "temperature_c": temperatures(), "presence": presence(), "allowed": allowed()}
    s["problems"] = problems(s)
    return s


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--cpus", help="a CPU list (taskset notation) the timings are pinned to")
    p.add_argument("--sample-s", type=float, default=1.0)
    p.add_argument("--check", action="store_true")
    args = p.parse_args()
    if not Path("/proc/self").exists():
        print(json.dumps({"host": os.environ.get("COMPUTERNAME"), "problems": ["not Linux: no host state"]}))
        return 1 if args.check else 0
    s = state(cpu_list(args.cpus) if args.cpus else None, args.sample_s)
    print(json.dumps(s, indent=1))
    return 1 if args.check and s["problems"] else 0


if __name__ == "__main__":
    sys.exit(main())
