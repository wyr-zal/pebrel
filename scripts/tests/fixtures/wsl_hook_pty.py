"""Opt-in WSL fixture: real PTY, bridge, Windows helper and kernel owner proof."""
import json
import os
from pathlib import Path
import sys

root = Path(os.environ["PEBREL_WSL_HOOK_TEST_ROOT"])
root.mkdir(parents=True, exist_ok=False)
source = os.environ["PEBREL_WSL_HOOK_TEST_SOURCE"]
child_code = r'''
import importlib.util, json, os, subprocess, sys, threading, time
from pathlib import Path
source = os.environ["PEBREL_WSL_HOOK_TEST_SOURCE"]
bridge = os.environ["PEBREL_WSL_HOOK_TEST_BRIDGE"]
helper = os.environ["PEBREL_HOOK_EXE"]
token = os.environ["PEBREL_WSL_HOOK_TOKEN"]
spec = importlib.util.spec_from_file_location("owner", Path(bridge).with_name("wsl_owner.py"))
owner_probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(owner_probe)
fields = Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()
request = dict(pane=os.environ["PEBREL_PANE_ID"], instance=os.environ["PEBREL_PROCESS_ID"], token=token, owner=f"{os.getpid()}:{fields[19]}", user=None, anchor=None)
proof = owner_probe.verify(request)
records = []
for entry in Path("/proc").iterdir():
    if not entry.name.isdigit(): continue
    try:
        if owner_probe.owned(entry, request):
            records.append(dict(stat=owner_probe.process(entry), argv0=owner_probe.argv(entry)[1]))
    except (OSError, ValueError, IndexError, StopIteration): pass
Path(os.environ["HOME"], "identity.json").write_text(json.dumps({"proof_ok": proof is not None, "argv": proof["argv"] if proof else None, "tty":fields[4], "foreground":fields[2] == fields[5], "records":records}))
bad = b"pebrel-wsl-hook/1 token=" + token.encode() + b"\nnebula-hook/1 source=" + source.encode() + b" process=1:1\n{}"
transport_env = os.environ.copy()
transport_env["WSLENV"] = ":".join([entry for entry in transport_env.get("WSLENV", "").split(":") if entry and entry.split("/", 1)[0] != "PEBREL_WSL_HOOK_PIPE"] + ["PEBREL_WSL_HOOK_PIPE/w"])
for wire in [bad, bad.replace(token.encode(), b"b" * 32, 1)]:
    result = subprocess.run([helper, "--wsl-hook"], input=wire, capture_output=True, timeout=3, env=transport_env)
    assert result.returncode == 0 and not result.stdout and not result.stderr
done = threading.Event()
def redraw():
    while not done.wait(0.005):
        os.write(1, b"\r\x1b[2Kfixture working")
redrawing = threading.Thread(target=redraw, daemon=True)
redrawing.start()
events = ["SessionStart", "UserPromptSubmit", "PostToolUse", "PreCompact", "PostCompact", "Stop"] if source == "claude" else ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop", "UserPromptSubmit", "Interrupt"]
for event in events:
    payload = dict(hook_event_name=event, session_id="fixture", turn_id="turn", prompt_id="turn", trigger="manual", tool_response="x" * 24000)
    if source == "codex" and event == "PreToolUse":
        payload.update(tool_name="request_user_input", tool_input={"questions":[{"question":"Fixture?"}]})
    args = ["/usr/bin/python3", bridge, source] + (["--hooks=full"] if source == "codex" else [])
    result = subprocess.run(args, input=json.dumps(payload).encode(), capture_output=True, timeout=4)
    assert result.returncode == 0 and not result.stdout and not result.stderr
done.set()
redrawing.join()
print("\n" + json.dumps(dict(child_ok=True)))
'''
script = root / "provider.py"
script.write_text(child_code)
env = os.environ.copy()
env.update(HOME=str(root), XDG_CACHE_HOME=str(root / "cache"), TMPDIR=str(root))
env["PEBREL_HOOK_LOG"] = str(root / "helper.ndjson")
env["WSLENV"] = env.get("WSLENV", "") + ":PEBREL_HOOK_LOG/pw"
# WSL already allocated the owning PTY. A second pty.fork() would create a
# different terminal beneath the credential-bearing root and must be rejected.
os.execve("/usr/bin/python3", [source, str(script)], env)
