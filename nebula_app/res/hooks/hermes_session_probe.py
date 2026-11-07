"""Read one verified Hermes process's native session metadata; never select by cwd/MRU."""
import json
import math
import os
from pathlib import Path
import re
import sqlite3
import sys
import time

MAX_BYTES = 64 * 1024


def read_bytes(path):
    with path.open("rb") as reader:
        value = reader.read(MAX_BYTES + 1)
    if len(value) > MAX_BYTES:
        raise ValueError("metadata exceeds limit")
    return value


def stat_fields(directory):
    return read_bytes(directory / "stat").decode().rsplit(")", 1)[1].split()


def probe(proc_root, pane, instance, pid, start, command_id=""):
    directory = proc_root / str(pid)
    before = stat_fields(directory)
    if before[0] in ("Z", "X") or before[19] != str(start):
        return None
    environment = dict(
        item.split(b"=", 1) for item in read_bytes(directory / "environ").split(b"\0") if b"=" in item
    )
    get = lambda key: os.fsdecode(environment.get(key.encode(), b""))
    if (get("PEBREL_PANE_ID") or get("NEBULA_PANE_ID")) != pane:
        return None
    if not instance or get("PEBREL_PROCESS_ID") != instance:
        return None
    if command_id and get("PEBREL_COMMAND_ID") != command_id:
        return None
    argv = read_bytes(directory / "cmdline").split(b"\0")
    roots = set()
    for index, fd in enumerate((directory / "fd").iterdir()):
        if index >= 128:
            return None
        try:
            target = fd.readlink()
        except OSError:
            continue
        if target.name == "state.db" and target.is_absolute():
            roots.add(target.parent)
    if not roots:
        # --profile is resolved inside Hermes, after the initial environ was
        # published. Without its open DB, guessing the default profile is unsafe.
        if any(word in (b"--profile", b"-p") or word.startswith(b"--profile=") for word in argv):
            return None
        home = get("HERMES_HOME") or str(Path(get("HOME")) / ".hermes")
        if not home or not Path(home).is_absolute():
            return None
        roots.add(Path(home))
    if len(roots) != 1:
        return None
    root = roots.pop()
    boot = next(
        float(line.split()[1]) for line in read_bytes(proc_root / "stat").decode().splitlines()
        if line.startswith("btime ")
    )
    started = boot + int(start) / os.sysconf("SC_CLK_TCK")
    candidates = set()
    # A breadcrumb is tied to this process's controlling tty and was written
    # after it started. A previous occupant of a recycled tty cannot claim it.
    for fd in ("0", "1"):
        try:
            descriptor = directory / "fd" / fd
            tty = str(descriptor.readlink())
            if not re.fullmatch(r"/dev/pts/\d+", tty) or descriptor.stat().st_rdev != int(before[4]):
                continue
            crumb = json.loads(read_bytes(root / "terminal-sessions" / ("tty-" + tty.strip("/").replace("/", "-"))))
            stamp = crumb.get("ts")
            if isinstance(stamp, bool) or not isinstance(stamp, (int, float)) or not math.isfinite(stamp):
                continue
            if started <= stamp <= time.time() and isinstance(crumb.get("session_id"), str):
                candidates.add(crumb["session_id"])
        except (OSError, ValueError, TypeError):
            continue
    # Hermes' active-session leases also bind an ID to PID + process creation.
    # This handles pipe-based frontends without accepting a shared WT_SESSION.
    try:
        registry = json.loads(read_bytes(root / "runtime" / "active_sessions.json"))
        entries = registry.get("entries", []) if isinstance(registry, dict) else []
        if not isinstance(entries, list) or len(entries) > 128:
            return None
        for entry in entries:
            if not isinstance(entry, dict) or entry.get("pid") != int(pid) or entry.get("surface") not in ("cli", "tui"):
                continue
            created = entry.get("process_start_time")
            if (isinstance(created, bool) or not isinstance(created, (int, float))
                    or not math.isfinite(created) or abs(created - started) > 0.01):
                continue
            metadata = entry.get("metadata") or {}
            session_id = metadata.get("live_session_id") if isinstance(metadata, dict) else None
            session_id = session_id or entry.get("session_id")
            if isinstance(session_id, str):
                candidates.add(session_id)
    except (OSError, ValueError, TypeError):
        pass
    if len(candidates) != 1:
        return None
    session_id = candidates.pop()
    if not session_id or len(session_id) > 128:
        return None
    connection = sqlite3.connect((root / "state.db").as_uri() + "?mode=ro", uri=True, timeout=0.1)
    try:
        if connection.execute("SELECT id FROM sessions WHERE id = ? LIMIT 1", (session_id,)).fetchone() is None:
            return None
    finally:
        connection.close()
    cwd = str((directory / "cwd").readlink())
    after = stat_fields(directory)
    if after[0] in ("Z", "X") or any(before[index] != after[index] for index in (1, 2, 3, 4, 5, 19)):
        return None
    return {"session_id": session_id, "cwd": cwd}


if __name__ == "__main__":
    try:
        result = probe(Path("/proc"), *sys.argv[1:])
        if result is not None:
            print(json.dumps(result, ensure_ascii=True))
    except (OSError, ValueError, IndexError, StopIteration, sqlite3.Error):
        sys.exit(1)
