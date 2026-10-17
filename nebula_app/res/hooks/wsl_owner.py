"""Read-only kernel evidence for one WSL pane; never consult its mutable default user."""
import json
import os
from pathlib import Path
import pwd
import re
import stat


def codex_failure(root, request, uid):
    """Read only a verified owner's open regular rollout, with bounded I/O."""
    target = request["failure"]
    session, turn = target["session"], target["turn"]
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
                if offset > len(first):
                    tail = tail.partition(b"\n")[2]
                candidates.append(tail)
        except (OSError, ValueError, TypeError, AttributeError):
            continue
    if len(candidates) != 1:
        return None
    failure = None
    for line in candidates[0].split(b"\n")[:-1]:
        try:
            record = json.loads(line)
            payload = record.get("payload", {})
            if record.get("type") != "event_msg":
                continue
            kind = payload.get("type")
            if kind in ("task_started", "task_complete", "turn_aborted"):
                failure = None
            if (kind == "task_complete" and payload.get("turn_id") == turn
                    and payload.get("root_turn_id") == turn and isinstance(payload.get("error"), dict)):
                message = payload["error"].get("message")
                if isinstance(message, str) and message.strip():
                    failure = message[:4000]
        except (ValueError, TypeError, AttributeError):
            return None
    return failure


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
    return dict(pid=int(root.name), parent=int(fields[1]), group=fields[2],
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
        if not evidence or process(proc / owner_pid) != owner or process(proc / str(root["pid"])) != root:
            return None
        result = dict(anchor=anchor, argv=evidence)
        if "failure" in request:
            result["failure"] = failure
        return result
    except (OSError, ValueError, IndexError, KeyError, StopIteration):
        return None
