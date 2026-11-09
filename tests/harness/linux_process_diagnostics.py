"""Bounded, failure-only wait evidence for an owned Linux fixture (#638)."""

from __future__ import annotations

import json
import shutil
import subprocess
import tempfile
from pathlib import Path


def _read(path: Path, limit: int = 1024) -> str:
    try:
        with path.open() as stream:
            value = stream.read(limit + 1)
        return value[:limit] + (" [truncated]" if len(value) > limit else "")
    except OSError as exc:
        return f"<{type(exc).__name__}: {exc}>"


def snapshot(pid: int, *, proc_root: Path = Path("/proc")) -> dict:
    """Read only the launched PID and descendants named by its task records."""
    pending = [pid]
    seen = set()
    processes = []
    while pending and len(processes) < 8:
        current = pending.pop(0)
        if current in seen:
            continue
        seen.add(current)
        process = proc_root / str(current)
        try:
            tasks = sorted((process / "task").iterdir(), key=lambda p: int(p.name))
        except OSError as exc:
            processes.append({"pid": current, "error": str(exc)})
            continue
        threads = []
        for task in tasks[:32]:
            children = _read(task / "children", 512)
            pending.extend(
                int(child) for child in children.split() if child.isdecimal()
            )
            threads.append(
                {
                    "tid": int(task.name),
                    "stat": _read(task / "stat", 512),
                    "wchan": _read(task / "wchan", 128),
                    "syscall": _read(task / "syscall", 256),
                    "stack": _read(task / "stack", 512),
                }
            )
        processes.append(
            {
                "pid": current,
                "status": _read(process / "status"),
                "io": _read(process / "io"),
                "threads": threads,
                "threads_truncated": len(tasks) > 32,
            }
        )
    return {"processes": processes, "processes_truncated": bool(pending)}


def collect(pid: int) -> str:
    """Snapshot before teardown, then sample syscalls for at most two seconds."""
    evidence = {"proc": snapshot(pid)}
    missing = [tool for tool in ("sudo", "timeout", "strace") if not shutil.which(tool)]
    if missing:
        evidence["trace"] = {"unavailable": missing}
    else:
        # Raw arguments avoid dumping read/write buffers. timeout owns strace's
        # lifetime even if the Python-side watchdog expires. No fixture retry.
        with tempfile.TemporaryFile() as output:
            try:
                result = subprocess.run(
                    [
                        "sudo",
                        "-n",
                        "timeout",
                        "--kill-after=0.25s",
                        "2s",
                        "strace",
                        "-f",
                        "-p",
                        str(pid),
                        "-e",
                        "trace=futex,poll,ppoll,epoll_wait,read,write,ioctl,openat,connect",
                        "-e",
                        "raw=all",
                    ],
                    stdout=subprocess.DEVNULL,
                    stderr=output,
                    timeout=3,
                    check=False,
                )
                evidence["trace_exit_code"] = result.returncode
            except (OSError, subprocess.TimeoutExpired) as exc:
                evidence["trace_error"] = f"{type(exc).__name__}: {exc}"
            output.seek(0, 2)
            size = output.tell()
            output.seek(max(0, size - 16384))
            evidence["trace"] = {
                "tail": output.read(16384).decode(errors="replace"),
                "truncated": size > 16384,
            }
    encoded = json.dumps(evidence)
    if len(encoded) > 49152:
        return json.dumps({"truncated": True, "prefix": encoded[:49152]})
    return encoded
