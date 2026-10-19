"""Read-only kernel evidence for one WSL pane; never consult its mutable default user."""
import json
import os
from pathlib import Path
import pwd
import re
import stat


def codex_terminal(root, request, uid):
    """Read terminal evidence only from one verified owner's stable open rollout."""
    target = request.get("terminal")
    if not isinstance(target, dict):
        return None
    session, turn = target.get("session"), target.get("turn")
    if not all(isinstance(value, str) and re.fullmatch(r"[0-9a-f-]{36}", value) for value in (session, turn)):
        return None
    candidates = []
    for index, fd in enumerate((root / "fd").iterdir()):
        if index >= 512:
            return None
        try:
            name = Path(os.readlink(fd)).name
            if not name.startswith("rollout-") or not name.endswith(f"-{session}.jsonl"):
                continue
            descriptor = os.open(fd, os.O_RDONLY | os.O_NONBLOCK)
            with os.fdopen(descriptor, "rb") as stream:
                info = os.fstat(stream.fileno())
                if not stat.S_ISREG(info.st_mode) or info.st_uid != uid:
                    continue
                first = stream.readline(65537)
                if len(first) > 65536 or not first.endswith(b"\n"):
                    continue
                meta = json.loads(first)
                payload = meta.get("payload", {})
                if (meta.get("type") != "session_meta" or payload.get("id") != session
                        or payload.get("source") != "cli" or payload.get("thread_source", "user") != "user"):
                    continue
                offset = max(len(first), info.st_size - 65536)
                stream.seek(offset)
                tail = stream.read(65536)
                after = os.fstat(stream.fileno())
                stable_fields = ("st_dev", "st_ino", "st_uid", "st_size", "st_mtime_ns", "st_ctime_ns")
                if any(getattr(info, field) != getattr(after, field) for field in stable_fields):
                    return None
                # A partial later start must not expose an earlier completed turn.
                if tail and not tail.endswith(b"\n"):
                    return None
                if offset > len(first):
                    tail = tail.partition(b"\n")[2]
                candidates.append((fd, info, tail))
        except (OSError, ValueError, TypeError, AttributeError):
            continue
    if len(candidates) != 1:
        return None
    selected_fd, info, tail = candidates[0]
    terminal = None
    for line in tail.split(b"\n")[:-1]:
        try:
            record = json.loads(line)
            payload = record.get("payload", {})
            if record.get("type") != "event_msg":
                continue
            kind = payload.get("type")
            if not isinstance(kind, str):
                return None
            if kind in ("task_started", "task_complete", "turn_aborted"):
                terminal = None
            if payload.get("turn_id") != turn or payload.get("root_turn_id") != turn:
                continue
            if kind == "task_complete":
                if "error" not in payload:
                    terminal = {"kind": "finished"}
                elif isinstance(payload["error"], dict):
                    message = payload["error"].get("message")
                    if isinstance(message, str) and message.strip():
                        terminal = {"kind": "failed", "error": message[:4000]}
            elif kind == "turn_aborted" and "error" not in payload:
                terminal = {"kind": "cancelled"}
        except (ValueError, TypeError, AttributeError):
            return None
    if terminal is None:
        return None
    # Our duplicated descriptor can survive the owner's close/rebind. Recheck the
    # owner's current map, including dup handles created after the first listing.
    try:
        current_fds = []
        for index, fd in enumerate((root / "fd").iterdir()):
            if index >= 512:
                return None
            name = Path(os.readlink(fd)).name
            mapped = fd.stat()
            same_file = (mapped.st_dev, mapped.st_ino) == (info.st_dev, info.st_ino)
            if same_file or (name.startswith("rollout-") and name.endswith(f"-{session}.jsonl")):
                current_fds.append(fd)
                if len(current_fds) > 1:
                    return None
        if current_fds != [selected_fd]:
            return None
        mapped = selected_fd.stat()
        if any(getattr(info, field) != getattr(mapped, field) for field in stable_fields):
            return None
    except OSError:
        return None
    return terminal


def codex_failure(root, request, uid):
    """Preserve the failure-only response for callers using the older request."""
    terminal = codex_terminal(root, {"terminal": request.get("failure")}, uid)
    return terminal["error"] if terminal and terminal["kind"] == "failed" else None


def read(path, limit):
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError("process evidence exceeds limit")
    return data


def process(root):
    raw = read(root / "stat", 4096).decode()
    fields = raw[raw.rfind(")") + 2:].split()
    uid = next(line.split()[1:5] for line in read(root / "status", 8192).decode().splitlines() if line.startswith("Uid:"))
    if len(set(uid)) != 1:
        raise ValueError("mixed process credentials")
    return dict(pid=int(root.name), state=fields[0], parent=int(fields[1]), group=fields[2],
                session=fields[3], tty=fields[4], foreground=fields[5],
                start=fields[19], uid=int(uid[0]))


def owned(root, request):
    env = dict(item.split(b"=", 1) for item in read(root / "environ", 65536).split(b"\0") if b"=" in item)
    return all(env.get(key.encode()) == str(request[field]).encode() for key, field in [
        ("PEBREL_PANE_ID", "pane"), ("PEBREL_PROCESS_ID", "instance"), ("PEBREL_WSL_HOOK_TOKEN", "token")])


def argv(root):
    args = read(root / "cmdline", 8192).split(b"\0")
    values = [read(root / "comm", 4096).decode().strip(), args[0].decode(), args[1].decode() if len(args) > 1 else ""]
    if any(len(value) > 4096 or any(ord(c) < 32 for c in value) for value in values):
        raise ValueError("invalid process arguments")
    return values


def verify(request, proc=Path("/proc")):
    try:
        if "terminal" in request and "failure" in request:
            return None
        match = re.fullmatch(r"([1-9][0-9]*):([0-9]+)", request["owner"])
        if match is None:
            return None
        owner_pid, expected_start = match.groups()
        owner = process(proc / owner_pid)
        if owner["start"] != expected_start or owner["tty"] == "0" or owner["group"] != owner["foreground"]:
            return None
        records = {}
        for index, root in enumerate(proc.iterdir()):
            if index >= 8192:
                return None
            if not root.name.isdigit():
                continue
            try:
                record = process(root)
                if record["tty"] != "0" and owned(root, request):
                    records[record["pid"]] = record
            except (OSError, ValueError, IndexError, StopIteration):
                continue
        roots = [record for record in records.values() if record["parent"] not in records]
        if len(roots) != 1:
            return None
        root = roots[0]
        anchor = {key: root[key] for key in ("pid", "start", "uid", "tty", "session")}
        if request.get("anchor") is not None and request["anchor"] != anchor:
            return None
        if request.get("user") and pwd.getpwnam(request["user"]).pw_uid != anchor["uid"]:
            return None
        evidence = []
        current = owner
        visited = set()
        while True:
            if current["pid"] in visited or len(visited) >= 48:
                return None
            visited.add(current["pid"])
            if any(current[key] != anchor[key] for key in ("uid", "tty", "session")):
                return None
            evidence.append(argv(proc / str(current["pid"])))
            if current["pid"] == root["pid"]:
                break
            current = records[current["parent"]]
        failure = codex_failure(proc / owner_pid, request, owner["uid"]) if "failure" in request else None
        terminal = codex_terminal(proc / owner_pid, request, owner["uid"]) if "terminal" in request else None
        if not evidence or process(proc / owner_pid) != owner or process(proc / str(root["pid"])) != root:
            return None
        result = dict(anchor=anchor, argv=evidence)
        if "failure" in request:
            result["failure"] = failure
        if "terminal" in request:
            result["terminal"] = terminal
        return result
    except (OSError, ValueError, IndexError, KeyError, StopIteration):
        return None


def owner_state(request, proc=Path("/proc")):
    """Classify an existing owner without treating infrastructure failures as exit."""
    try:
        match = re.fullmatch(r"([1-9][0-9]*):([0-9]+)", request.get("owner", ""))
        if match is None:
            return "unavailable"
        owner_pid, expected_start = match.groups()
        root = proc / owner_pid
        try:
            before = process(root)
        except (FileNotFoundError, ProcessLookupError):
            return "ended"
        if before["start"] != expected_start:
            return "ended"
        if before["state"] in ("Z", "X") or before["tty"] == "0":
            return "ended"
        if before["group"] != before["foreground"]:
            return "ended"
        evidence = verify(request, proc)
        if evidence is not None:
            return evidence
        try:
            after = process(root)
        except (FileNotFoundError, ProcessLookupError):
            return "ended"
        if after["start"] != before["start"] or after["state"] in ("Z", "X"):
            return "ended"
        if after["tty"] == "0" or after["group"] != after["foreground"]:
            return "ended"
        return "unavailable"
    except (OSError, ValueError, IndexError, KeyError, StopIteration):
        return "unavailable"
