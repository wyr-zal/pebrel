"""Real PTY delivery and transactional SSH file-adapter regressions."""
import base64
import importlib.util
import json
import os
from pathlib import Path
import re
import select
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ASSETS = Path(__file__).resolve().parents[2] / "nebula_app/res/hooks"
TOKEN = "0123456789abcdef0123456789abcdef"


def module(name):
    spec = importlib.util.spec_from_file_location(name, ASSETS / (name + ".py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


@unittest.skipUnless(os.name == "posix", "remote adapter targets POSIX SSH hosts")
class RemoteHooksTests(unittest.TestCase):
    def test_wsl_hook_transport_uses_helper_stdin_without_terminal_output(self):
        import io
        bridge = module("remote_bridge")
        payload = {"hook_event_name": "PostCompact", "session_id": "fixture",
                   "prompt_id": "compact", "trigger": "manual", "compact_summary": "x" * 24000}
        helper = self.root / "helper"
        helper.write_text("#!/usr/bin/python3\nimport sys\nfrom pathlib import Path\n"
                          "Path(__file__).with_suffix('.wire').write_bytes(sys.stdin.buffer.read())\n")
        helper.chmod(0o700)
        output = io.StringIO()
        env = {"PEBREL_WSL_HOOK_PIPE": "fixture-pipe", "PEBREL_WSL_HOOK_TOKEN": TOKEN,
               "PEBREL_HOOK_EXE": str(helper), "WSLENV": "USER_VALUE/p"}
        with patch.dict(os.environ, env), patch.object(bridge, "process_identity", return_value="41:70"), patch.object(bridge, "write_terminal") as terminal, patch.object(bridge.sys, "stdout", output):
            bridge.send("claude", False, "", payload, TOKEN)
            self.assertFalse(terminal.called, "WSL hook must not write to the terminal")
        wire = helper.with_suffix(".wire").read_bytes()
        prefix, envelope = wire.split(b"\n", 1)
        self.assertEqual(prefix, b"pebrel-wsl-hook/1 token=" + TOKEN.encode())
        header, body = envelope.split(b"\n", 1)
        self.assertIn(b"source=claude process=41:70", header)
        self.assertEqual(json.loads(body)["compact_summary"], payload["compact_summary"])
        self.assertNotIn(b"\x1b", wire)
        self.assertEqual(output.getvalue(), "")

    def test_wsl_hook_unavailable_never_falls_back_and_preserves_notifier(self):
        import io
        bridge = module("remote_bridge")
        env = {"PEBREL_WSL_HOOK_PIPE": "fixture", "PEBREL_WSL_HOOK_TOKEN": TOKEN,
               "PEBREL_HOOK_EXE": "/missing/helper"}
        with patch.dict(os.environ, env), patch.object(bridge, "write_terminal") as terminal, patch.object(bridge.subprocess, "run", side_effect=OSError("missing helper")), patch.object(bridge.subprocess, "Popen") as notifier:
            with self.assertRaises(OSError):
                bridge.run(["codex", "--chain", "original-notifier", '{}'])
            self.assertFalse(terminal.called, "transport failure must not write OSC")
            notifier.assert_called_once()

    def test_wsl_hook_preserves_wslenv_but_overrides_pipe_direction(self):
        bridge = module("remote_bridge")
        env = {"PEBREL_WSL_HOOK_PIPE": "fixture", "PEBREL_WSL_HOOK_TOKEN": TOKEN,
               "PEBREL_HOOK_EXE": "/fixture/helper", "WSLENV": "USER_VALUE/p:PEBREL_WSL_HOOK_PIPE/p"}
        with patch.dict(os.environ, env), patch.object(bridge.subprocess, "run") as helper:
            bridge.write_wsl_hook(b"fixture")
            self.assertEqual(helper.call_args.kwargs["env"]["WSLENV"], "USER_VALUE/p:PEBREL_WSL_HOOK_PIPE/w")
            self.assertNotIn(TOKEN, repr(helper.call_args.args))

    def test_wsl_hook_concurrent_events_wait_for_authenticated_delivery(self):
        import threading
        bridge = module("remote_bridge")
        helper = self.root / "slow-helper"
        helper.write_text("#!/usr/bin/python3\nimport json, sys, time\nfrom pathlib import Path\n"
                          "wire=sys.stdin.buffer.read()\ntime.sleep(0.45)\n"
                          "sequence=json.loads(wire.split(b'\\n',2)[2])['bridge_sequence']\n"
                          "Path(str(Path(__file__))+str(sequence)).write_bytes(wire)\n")
        helper.chmod(0o700)
        errors = []
        def deliver():
            try:
                bridge.send("claude", False, "", {"hook_event_name":"Stop", "session_id":"fixture"}, TOKEN)
            except Exception as error:
                errors.append(type(error).__name__)
        env = {"PEBREL_WSL_HOOK_PIPE":"fixture", "PEBREL_WSL_HOOK_TOKEN":TOKEN, "PEBREL_HOOK_EXE":str(helper)}
        with patch.dict(os.environ, env), patch.object(bridge, "process_identity", return_value="41:70"), patch.object(bridge, "write_terminal") as terminal:
            first = threading.Thread(target=deliver)
            second = threading.Thread(target=deliver)
            first.start()
            time.sleep(0.03)
            second.start()
            first.join(3)
            second.join(3)
            self.assertFalse(first.is_alive() or second.is_alive())
            self.assertFalse(errors, f"concurrent lifecycle event lost: {errors}")
            self.assertFalse(terminal.called)
        self.assertEqual(len(list(self.root.glob("slow-helper[12]"))), 2)

    def test_claude_without_controlling_tty_returns_terminal_sequence(self):
        import errno
        import io
        bridge = module("remote_bridge")
        osc = b"\x1b]777;nebula-hook;fixture;payload\x07"
        output = io.StringIO()
        with patch.object(bridge.os, "open", side_effect=OSError(errno.ENXIO, "no tty")), patch.object(bridge.sys, "stdout", output):
            bridge.write_terminal(osc, "claude")
        self.assertEqual(json.loads(output.getvalue()), {"terminalSequence": osc.decode("ascii")})

    def test_stdout_fallback_is_only_for_claude_without_a_tty(self):
        import errno
        import io
        bridge = module("remote_bridge")
        for source, error in [("codex", errno.ENXIO), ("pi", errno.ENXIO), ("opencode", errno.ENXIO), ("claude", errno.EACCES)]:
            with self.subTest(source=source, error=error), patch.object(bridge.os, "open", side_effect=OSError(error, "unavailable")), patch.object(bridge.sys, "stdout", io.StringIO()) as output:
                with self.assertRaises(OSError):
                    bridge.write_terminal(b"\x07", source)
                self.assertEqual(output.getvalue(), "")

    def test_claude_with_controlling_tty_does_not_duplicate_stdout(self):
        import io
        bridge = module("remote_bridge")
        with patch.object(bridge.os, "open", return_value=42), patch.object(bridge.os, "close"), patch.object(bridge.select, "select", return_value=([], [42], [])), patch.object(bridge.os, "write", return_value=1) as write, patch.object(bridge.sys, "stdout", io.StringIO()) as output:
            bridge.write_terminal(b"\x07", "claude")
            write.assert_called_once_with(42, b"\x07")
            self.assertEqual(output.getvalue(), "")

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="pebrel-remote-hooks-")
        self.addCleanup(self.tmp.cleanup)
        # macOS exposes its temporary directory through a system symlink.
        # Ordinary fixture paths must exercise transactions, while redirection
        # is supplied explicitly by the negative tests below.
        self.root = Path(self.tmp.name).resolve()
        env = {"HOME": str(self.root), "PATH": os.environ["PATH"], "SHELL": "/bin/bash",
               "XDG_DATA_HOME": str(self.root / "data"), "XDG_CACHE_HOME": str(self.root / "cache"),
               "PEBREL_REMOTE_HOOK_TOKEN": TOKEN}
        self.environment = patch.dict(os.environ, env, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def test_discovery_finds_nvm_cli_and_its_interpreter_without_login_path(self):
        files = module("remote_files")
        bindir = self.root / ".nvm/versions/node/v24.18.0/bin"
        bindir.mkdir(parents=True)
        node = bindir / "node"
        node.write_text("#!/bin/sh\nprintf 'hooks stable true\\n'\n")
        node.chmod(0o700)
        cli = bindir / "codex"
        cli.write_text("#!/usr/bin/env node\n")
        cli.chmod(0o700)
        with patch.dict(os.environ, {"PATH": "/usr/bin:/bin"}):
            self.assertEqual(files.find_program("codex"), str(cli))
            self.assertEqual(files.capture(str(cli), "features", "list"), "hooks stable true\n")
            configured = self.root / "bin"
            configured.mkdir()
            preferred = configured / "codex"
            preferred.write_text("#!/bin/sh\n")
            preferred.chmod(0o700)
            with patch.dict(os.environ, {"PATH": str(configured)}):
                self.assertEqual(files.find_program("codex"), str(preferred))

    def test_compare_and_swap_does_not_overwrite_concurrent_user_edit(self):
        files = module("remote_files")
        snapshot = files.snapshot()
        root, paths = files.locations()
        paths["claude"].parent.mkdir(parents=True)
        paths["claude"].write_text("user change")
        plan = [{"name": "claude", "expected": snapshot["files"]["claude"]["sha256"], "content": "new"}]
        with self.assertRaisesRegex(ValueError, "changed during setup"):
            files.apply(plan)
        self.assertEqual(paths["claude"].read_text(), "user change")

    def test_transaction_rolls_back_only_its_writes(self):
        files = module("remote_files")
        real_replace = files.replace
        root, paths = files.locations()
        def replace(path, content, executable=False):
            if path == paths["bridge.py"]:
                raise OSError("fixture write failure")
            real_replace(path, content, executable)
        plan = [{"name": name, "expected": None, "content": "new"} for name in ["pebrel-hook", "bridge.py"]]
        with patch.object(files, "replace", replace), self.assertRaises(OSError):
            files.apply(plan)
        self.assertFalse(paths["pebrel-hook"].exists())
        self.assertFalse(paths["bridge.py"].exists())

    def test_unknown_name_and_symlink_cannot_redirect_writes(self):
        files = module("remote_files")
        with self.assertRaises(ValueError):
            files.apply([{"name": "../outside", "expected": None, "content": "bad"}])
        root, paths = files.locations()
        outside = self.root / "outside"
        outside.write_text("preserve")
        paths["bridge.py"].symlink_to(outside)
        with self.assertRaises(ValueError):
            files.apply([{"name": "bridge.py", "expected": None, "content": "bad"}])
        self.assertEqual(outside.read_text(), "preserve")

    def test_symlinked_integration_directory_cannot_redirect_writes(self):
        files = module("remote_files")
        root, paths = files.locations()
        outside = self.root / "outside"
        outside.mkdir()
        root.parent.mkdir(parents=True)
        root.symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "symbolic link"):
            files.apply([{"name": "bridge.py", "expected": None, "content": "bad"}])
        self.assertEqual(list(outside.iterdir()), [])

    def test_bsd_ancestry_recognizes_interpreted_provider_without_trusting_other_arguments(self):
        bridge = module("remote_bridge")
        prefix = "400 Sat Sep 19 12:00:00 2026 "
        for source, command in [
            ("codex", "/usr/bin/python3 /private/var/fixture/codex"),
            ("claude", "/usr/bin/node /opt/modules/@anthropic-ai/claude-code/cli.js"),
            ("pi", "/usr/bin/node /opt/modules/pi-coding-agent/dist/cli.js"),
        ]:
            def ps_output(args, **kwargs):
                observed = command if "args=" in args else command.split()[0]
                return (prefix + observed).encode()
            with self.subTest(source=source), patch.object(bridge.os, "getppid", return_value=500), patch.object(bridge.Path, "read_text", side_effect=FileNotFoundError), patch.object(bridge.subprocess, "check_output", side_effect=ps_output):
                self.assertRegex(bridge.process_identity(source), r"^500:[a-f0-9]{16}$")
        row = b"1 Sat Sep 19 12:00:00 2026 /usr/bin/python3 worker.py unrelated codex"
        with patch.object(bridge.os, "getppid", return_value=500), patch.object(bridge.Path, "read_text", side_effect=FileNotFoundError), patch.object(bridge.subprocess, "check_output", return_value=row):
            self.assertIsNone(bridge.process_identity("codex"))

    def test_installed_launcher_delivers_native_stdin_over_real_controlling_tty(self):
        import pty
        provider = self.root / "codex"
        bridge = ASSETS / "remote_bridge.py"
        # Parent ancestry supplies process identity, never a PID from JSON.
        provider.write_text(
            "import json, subprocess, sys\n"
            f"bridge={str(bridge)!r}\n"
            "for event in ['SessionStart','UserPromptSubmit','PermissionRequest','PreToolUse','Stop']:\n"
            " p={'hook_event_name':event,'session_id':'main','turn_id':'turn','pid':1, 'transcript_path':'/home/user/.codex/sessions/rollout-date-0199a213-c2a4-7cf5-8f6b-d746fbb6e86c.jsonl'}\n"
            " if event == 'PreToolUse': p.update(tool_name='request_user_input',tool_input={'questions':[{'id':'scope','question':'Which scope?'}]})\n"
            " if event == 'Stop': p['last_assistant_message']='x'*70000\n"
            " subprocess.run([sys.executable,bridge,'codex','--hooks=full'],input=json.dumps(p).encode(),check=True)\n"
        )
        pid, master = pty.fork()
        if pid == 0:
            os.execv(sys.executable, [sys.executable, str(provider)])
        output = b""
        try:
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        part = os.read(master, 65536)
                    except OSError:
                        break
                    if not part:
                        break
                    output += part
            waited, status = os.waitpid(pid, 0)
            self.assertEqual(status, 0, output.decode(errors="replace"))
        finally:
            os.close(master)
        envelopes = re.findall(rb"\x1b\]777;nebula-hook;" + TOKEN.encode() + rb";([^\x07]+)\x07", output)
        self.assertEqual(len(envelopes), 5, output[:2000])
        decoded = [base64.b64decode(value).split(b"\n", 1) for value in envelopes]
        for header, payload in decoded:
            self.assertIn(f"process={pid}:".encode(), header)
            self.assertIn(b"codex_hooks=full", header)
            self.assertLessEqual(len(header) + len(payload) + 1, 65536)
        payloads = [json.loads(payload) for _, payload in decoded]
        self.assertEqual([p["bridge_sequence"] for p in payloads], [1, 2, 3, 4, 5])
        self.assertEqual(payloads[3]["tool_input"]["questions"][0]["question"], "Which scope?")
        self.assertEqual(payloads[-1]["hook_event_name"], "Stop")
        self.assertEqual(payloads[-1]["transcript_path"], payloads[0]["transcript_path"])
        self.assertNotIn("last_assistant_message", payloads[-1])

    def test_codex_hook_without_controlling_tty_uses_owning_provider_pty(self):
        import pty
        provider = self.root / "codex"
        bridge = ASSETS / "remote_bridge.py"
        provider.write_text(
            "import json, subprocess, sys\n"
            f"bridge={str(bridge)!r}\n"
            "payload={'hook_event_name':'UserPromptSubmit','session_id':'main'}\n"
            "result=subprocess.run([sys.executable,bridge,'codex','--hooks=full'],\n"
            " input=json.dumps(payload).encode(),stdout=subprocess.PIPE,stderr=subprocess.PIPE,\n"
            " start_new_session=True,check=True)\n"
            "assert result.stdout == b'', result.stdout\n"
            "assert result.stderr == b'', result.stderr\n"
        )
        pid, master = pty.fork()
        if pid == 0:
            os.execv(sys.executable, [sys.executable, str(provider)])
        output = b""
        try:
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        part = os.read(master, 65536)
                    except OSError:
                        break
                    if not part:
                        break
                    output += part
            waited, status = os.waitpid(pid, 0)
            self.assertEqual(status, 0, output.decode(errors="replace"))
        finally:
            os.close(master)
        envelopes = re.findall(rb"\x1b\]777;nebula-hook;" + TOKEN.encode() + rb";([^\x07]+)\x07", output)
        self.assertEqual(len(envelopes), 1, output[:2000])
        header, payload = base64.b64decode(envelopes[0]).split(b"\n", 1)
        self.assertIn(f"process={pid}:".encode(), header)
        self.assertIn(b"codex_hooks=full", header)
        self.assertEqual(json.loads(payload)["hook_event_name"], "UserPromptSubmit")

    def test_large_claude_failure_keeps_prompt_identity_and_error(self):
        bridge = module("remote_bridge")
        payload = {"hook_event_name": "StopFailure", "session_id": "main",
                   "prompt_id": "current-prompt", "error": "unknown",
                   "last_assistant_message": "x" * 70000}
        with patch.object(bridge, "write_terminal") as write:
            bridge.send("claude", False, "", payload, TOKEN)
        frame, source, _owner = write.call_args.args
        self.assertEqual(source, "claude")
        encoded = frame.split(b";", 3)[3].removesuffix(b"\x07")
        body = json.loads(base64.b64decode(encoded).split(b"\n", 1)[1])
        self.assertEqual(body["prompt_id"], "current-prompt")
        self.assertEqual(body["error"], "unknown")
        self.assertNotIn("last_assistant_message", body)

    def test_large_compaction_keeps_trigger_and_prompt_but_not_summary(self):
        bridge = module("remote_bridge")
        for event, field in [("PreCompact", "custom_instructions"), ("PostCompact", "compact_summary")]:
            payload = {"hook_event_name": event, "session_id": "main",
                       "prompt_id": "compact-prompt", "trigger": "manual", field: "x" * 70000}
            with patch.object(bridge, "write_terminal") as write:
                bridge.send("claude", False, "", payload, TOKEN)
            frame = write.call_args.args[0]
            encoded = frame.split(b";", 3)[3].removesuffix(b"\x07")
            body = json.loads(base64.b64decode(encoded).split(b"\n", 1)[1])
            self.assertEqual(body.get("trigger"), "manual")
            self.assertEqual(body["prompt_id"], "compact-prompt")
            self.assertNotIn(field, body)

    def test_compaction_without_tty_uses_foreground_claude_pty_without_stdout(self):
        import pty
        provider = self.root / "claude"
        bridge = ASSETS / "remote_bridge.py"
        provider.write_text(
            "import json, subprocess, sys\n"
            "for event in ['PreCompact', 'PostCompact']:\n"
            " payload = {'hook_event_name': event, 'session_id': 'fixture', 'prompt_id': 'compact', 'trigger': 'manual'}\n"
            " result = subprocess.run([sys.executable, sys.argv[1], 'claude'], input=json.dumps(payload), text=True, capture_output=True, start_new_session=True, timeout=5)\n"
            " assert result.returncode == 0\n"
            " assert not result.stdout, 'compaction hook JSON must not become visible CLI text'\n"
            " assert not result.stderr\n"
        )
        env = dict(os.environ, PEBREL_PANE_ID="31", PEBREL_PROCESS_ID="32")
        pid, master = pty.fork()
        if pid == 0:
            os.execve(sys.executable, [sys.executable, str(provider), str(bridge)], env)
        output = b""
        try:
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                if not select.select([master], [], [], 0.1)[0]:
                    continue
                try:
                    part = os.read(master, 65536)
                except OSError:
                    break
                if not part:
                    break
                output += part
            _, status = os.waitpid(pid, 0)
            self.assertEqual(status, 0, output.decode(errors="replace"))
        finally:
            os.close(master)
        frames = re.findall(rb"\x1b\]777;nebula-hook;" + TOKEN.encode() + rb";([^\x07]+)\x07", output)
        self.assertEqual(len(frames), 2)
        for raw in frames:
            header, payload = base64.b64decode(raw).split(b"\n", 1)
            self.assertIn(f"process={pid}:".encode(), header)
            self.assertEqual(json.loads(payload)["prompt_id"], "compact")

    def test_compaction_rejects_stale_pid_foreign_scope_and_background_owner(self):
        bridge = module("remote_bridge")
        fields = ["S", "1", "41", "41", "123", "41"] + ["0"] * 13 + ["70"]
        good_stat = "41 (claude) " + " ".join(fields)
        background_stat = good_stat.replace("S 1 41 41 123 41", "S 1 41 41 123 99")
        stale_stat = good_stat[:-2] + "71"
        scope = {"PEBREL_PANE_ID": "31", "PEBREL_PROCESS_ID": "32", "PEBREL_REMOTE_HOOK_TOKEN": TOKEN}
        good_env = b"\0".join(f"{key}={value}".encode() for key, value in scope.items())
        cases = [(stale_stat, good_env, 123), (background_stat, good_env, 123),
                 (good_stat, good_env.replace(b"ID=31", b"ID=99"), 123),
                 (good_stat, good_env.replace(b"ID=32", b"ID=99"), 123),
                 (good_stat, good_env.replace(TOKEN.encode(), b"f" * 32), 123),
                 (good_stat, good_env, 999)]
        with patch.dict(os.environ, scope):
            for raw, env, device in cases:
                from types import SimpleNamespace
                with self.subTest(raw=raw, device=device), patch.object(bridge.Path, "read_text", return_value=raw), patch.object(bridge.Path, "read_bytes", return_value=env), patch.object(bridge, "open_owner_terminal", return_value=42), patch.object(bridge.os, "fstat", return_value=SimpleNamespace(st_rdev=device)), patch.object(bridge.os, "close"):
                    self.assertIsNone(bridge.open_claude_compact_terminal("41:70"))
            with patch.object(bridge.Path, "read_text", side_effect=[good_stat, stale_stat]), patch.object(bridge.Path, "read_bytes", return_value=good_env), patch.object(bridge, "open_owner_terminal", return_value=42), patch.object(bridge.os, "close") as close:
                self.assertIsNone(bridge.open_claude_compact_terminal("41:70"))
                close.assert_called_once_with(42)

    def test_large_pi_events_keep_the_native_file_and_bridge_lifetime(self):
        bridge = module("remote_bridge")
        payload = {"kind": "done", "session_id": "pi-native", "session_file": "/sessions/pi.jsonl",
                   "bridge_instance": "process-start", "stop_reason": "stop",
                   "last_assistant_message": "x" * 70000}
        with patch.object(bridge, "write_terminal") as write:
            bridge.send("pi", False, "", payload, TOKEN)
        frame = write.call_args.args[0]
        encoded = frame.split(b";", 3)[3].removesuffix(b"\x07")
        body = json.loads(base64.b64decode(encoded).split(b"\n", 1)[1])
        for key in ["session_id", "session_file", "bridge_instance", "stop_reason"]:
            self.assertEqual(body[key], payload[key])
        self.assertNotIn("last_assistant_message", body)

    def test_invalid_token_never_writes_and_failed_delivery_keeps_notify_chain(self):
        bridge = module("remote_bridge")
        with patch.dict(os.environ, {"PEBREL_REMOTE_HOOK_TOKEN": "bad"}), patch.object(bridge, "send") as send:
            bridge.run(["codex", '{"type":"agent-turn-complete"}'])
            send.assert_not_called()
        for payload in ['{"type":"agent-turn-complete"}', "malformed"]:
            with patch.object(bridge, "send", side_effect=OSError("no tty")), patch.object(bridge.subprocess, "Popen") as popen:
                with self.assertRaises((OSError, ValueError)):
                    bridge.run(["codex", "--chain", "user-notifier", "original-arg", payload])
                self.assertEqual(popen.call_args.args[0], ["user-notifier", "original-arg", payload])

    def test_partial_terminal_frame_is_cancelled_on_timeout(self):
        bridge = module("remote_bridge")
        writes = []
        def write(fd, value):
            writes.append(value)
            return 2 if len(writes) == 1 else len(value)
        with patch.object(bridge.os, "open", return_value=5), patch.object(bridge.os, "close"), patch.object(bridge.os, "write", write), patch.object(bridge.select, "select", side_effect=[([], [5], []), ([], [], []), ([], [5], [])]):
            with self.assertRaises(TimeoutError):
                bridge.write_terminal(b"\x1b]777;partial\x07")
        self.assertEqual(writes[-1], b"\x18")


if __name__ == "__main__":
    unittest.main()
