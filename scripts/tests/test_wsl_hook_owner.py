"""Kernel evidence contract for the dedicated WSL hook channel."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock


@unittest.skipUnless(os.name == "posix", "WSL owner probe uses Linux /proc and pwd")
class WslHookOwnerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.request = dict(pane="17", instance="900", token="a" * 32, owner="20:200", user=None, anchor=None)
        self.process(10, 1, 100, "bash")
        self.process(20, 10, 200, "claude")

    def process(self, pid, parent, start, name, uid=1000, tty=42, group=20, foreground=20):
        root = self.root / str(pid)
        root.mkdir(exist_ok=True)
        fields = ["S", str(parent), str(group), "10", str(tty), str(foreground)] + ["0"] * 13 + [str(start)]
        (root / "stat").write_text(f"{pid} ({name}) " + " ".join(fields))
        (root / "status").write_text(f"Uid:\t{uid}\t{uid}\t{uid}\t{uid}\n")
        (root / "cmdline").write_bytes(name.encode() + b"\0")
        (root / "comm").write_text(name)
        env = {"PEBREL_PANE_ID": "17", "PEBREL_PROCESS_ID": "900", "PEBREL_WSL_HOOK_TOKEN": "a" * 32}
        (root / "environ").write_bytes(b"\0".join(f"{k}={v}".encode() for k, v in env.items()))

    def verify(self):
        path = Path(__file__).resolve().parents[2] / "nebula_app/res/hooks/wsl_owner.py"
        spec = importlib.util.spec_from_file_location("wsl_owner", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module.verify(self.request, self.root)

    def test_wsl_hook_freezes_real_pty_root_and_preserves_process_evidence(self):
        result = self.verify()
        self.assertEqual(result["anchor"], dict(pid=10, start="100", uid=1000, tty="42", session="10"))
        self.assertEqual(result["argv"][0], ["claude", "claude", ""])
        self.request["anchor"] = result["anchor"]
        self.assertEqual(self.verify(), result)

    def test_wsl_hook_rejects_wrong_credentials_pid_epoch_and_foreign_user(self):
        for key, wrong in [("pane", "18"), ("instance", "901"), ("token", "b" * 32), ("owner", "20:201"), ("owner", "21:200")]:
            with self.subTest(key=key):
                saved = self.request[key]
                self.request[key] = wrong
                self.assertIsNone(self.verify())
                self.request[key] = saved
        self.process(20, 10, 200, "claude", uid=1001)
        self.assertIsNone(self.verify())
        self.request["owner"] = "1:1"
        self.assertIsNone(self.verify())

    def test_wsl_hook_rejects_replaced_root_ambiguous_panes_and_detached_owners(self):
        self.request["anchor"] = self.verify()["anchor"]
        self.process(10, 1, 101, "bash")
        self.assertIsNone(self.verify())
        self.process(10, 1, 100, "bash")
        self.process(20, 10, 200, "claude", tty=0)
        self.assertIsNone(self.verify())
        self.process(20, 10, 200, "claude", foreground=999)
        self.assertIsNone(self.verify())
        self.process(20, 10, 200, "claude")
        self.process(30, 1, 300, "bash", tty=43)
        self.assertIsNone(self.verify())

    def test_wsl_hook_does_not_follow_another_distribution_or_changed_user(self):
        self.request["anchor"] = self.verify()["anchor"]
        self.process(10, 1, 100, "bash", uid=1001)
        self.process(20, 10, 200, "claude", uid=1001)
        self.assertIsNone(self.verify())

    def test_wsl_hook_supports_a_provider_execed_in_place_of_the_shell(self):
        self.request["owner"] = "10:100"
        self.process(10, 1, 100, "claude", group=10, foreground=10)
        result = self.verify()
        self.assertIsNotNone(result)
        self.assertEqual(result["argv"][0][0], "claude")

    def test_wsl_hook_codex_failure_requires_owned_open_main_rollout_and_exact_turn(self):
        session = "01a14552-4a29-7353-8de1-2b37cd372138"
        turn = "01a14552-8719-7370-bd63-c56b8976ed31"
        self.process(10, 1, 100, "bash", uid=os.getuid())
        self.process(20, 10, 200, "codex", uid=os.getuid())
        self.request["anchor"] = self.verify()["anchor"]
        self.request["failure"] = dict(session=session, turn=turn)
        fd = self.root / "20/fd"
        fd.mkdir()
        rollout = self.root / f"rollout-date-{session}.jsonl"
        (fd / "7").symlink_to(rollout)
        meta = dict(type="session_meta", payload=dict(id=session, source="cli", thread_source="user",
                    base_instructions={"text": "x" * 24000}))
        complete = dict(type="event_msg", payload=dict(type="task_complete", turn_id=turn,
                        root_turn_id=turn, error=dict(message="temporary failure", codex_error_info="internal_server_error")))

        def write(*records, suffix=""):
            rollout.write_text("\n".join(json.dumps(record) for record in records) + suffix)

        write(meta, complete, suffix="\n")
        self.assertEqual(self.verify().get("failure"), "temporary failure")
        meta["payload"]["base_instructions"]["text"] = "x" * 65536
        write(meta, complete, suffix="\n")
        self.assertIsNone(self.verify().get("failure"), "metadata reads remain bounded")
        meta["payload"]["base_instructions"]["text"] = "x" * 24000
        for changed in [dict(turn="old"), dict(session="foreign")]:
            saved = self.request["failure"].copy()
            self.request["failure"].update(changed)
            self.assertIsNone(self.verify().get("failure"))
            self.request["failure"] = saved
        complete["payload"]["root_turn_id"] = "subagent"
        write(meta, complete, suffix="\n")
        self.assertIsNone(self.verify().get("failure"))
        complete["payload"]["root_turn_id"] = turn
        write(meta, complete)  # A partially appended record is not a fact yet.
        self.assertIsNone(self.verify().get("failure"))
        write(meta, complete, dict(type="event_msg", payload=dict(type="task_started", turn_id="next")), suffix="\n")
        self.assertIsNone(self.verify().get("failure"))
        for malformed in [[], dict(type="session_meta", payload=[])]:
            write(malformed, complete, suffix="\n")
            self.assertIsNone(self.verify().get("failure"))
        meta["payload"]["thread_source"] = "guardian"
        write(meta, complete, suffix="\n")
        self.assertIsNone(self.verify().get("failure"))
        meta["payload"]["thread_source"] = "user"
        write(meta, complete, suffix="\n")
        (fd / "8").symlink_to(rollout)
        self.assertIsNone(self.verify().get("failure"), "ambiguous handles fail closed")
        self.request["owner"] = "20:201"
        self.assertIsNone(self.verify(), "PID reuse cannot read or apply a failure")

    def terminal_rollout(self):
        self.process(10, 1, 100, "bash", uid=os.getuid())
        self.process(20, 10, 200, "codex", uid=os.getuid())
        self.request["anchor"] = self.verify()["anchor"]
        self.request["terminal"] = dict(session="01a14552-4a29-7353-8de1-2b37cd372138",
                                        turn="01a14552-8719-7370-bd63-c56b8976ed31")
        target = self.request["terminal"]
        self.rollout = self.root / f"rollout-date-{target['session']}.jsonl"
        self.fd = self.root / "20/fd"
        self.fd.mkdir()
        (self.fd / "7").symlink_to(self.rollout)
        self.meta = dict(type="session_meta", payload=dict(id=target["session"], source="cli",
                         thread_source="user", base_instructions={"text": "x" * 24000}))

    def terminal_record(self, kind="task_complete", **fields):
        turn = self.request["terminal"]["turn"]
        return dict(type="event_msg", payload=dict(type=kind, turn_id=turn, root_turn_id=turn, **fields))

    def write_terminal(self, *records, suffix="\n"):
        self.rollout.write_text("\n".join(json.dumps(record) for record in (self.meta, *records)) + suffix)

    def test_wsl_hook_terminal_complete_and_abort_only_prove_current_turn_stopped(self):
        self.terminal_rollout()
        for kind, expected in [("task_complete", "finished"), ("turn_aborted", "cancelled")]:
            with self.subTest(kind=kind):
                self.write_terminal(self.terminal_record(kind))
                result = self.verify()
                self.assertIsNotNone(result)
                self.assertEqual(result.get("terminal"), {"kind": expected})
                self.assertNotIn("failure", result)

    def test_wsl_hook_terminal_failure_keeps_legacy_failure_contract(self):
        self.terminal_rollout()
        self.write_terminal(self.terminal_record(error={"message": "failure " * 1000}))
        expected = ("failure " * 1000)[:4000]
        self.assertEqual(self.verify().get("terminal"), {"kind": "failed", "error": expected})
        target = self.request.pop("terminal")
        self.request["failure"] = target
        self.assertEqual(self.verify().get("failure"), expected)
        self.request["terminal"] = self.request.pop("failure")
        self.write_terminal(self.terminal_record())
        self.request["failure"] = self.request.pop("terminal")
        self.assertIsNone(self.verify().get("failure"))

    def test_wsl_hook_terminal_rejects_malformed_error_and_unbound_target(self):
        self.terminal_rollout()
        for error in [None, [], "failure", {}, {"message": ""}, {"message": "  "}, {"message": 7}]:
            with self.subTest(error=error):
                self.write_terminal(self.terminal_record(error=error))
                self.assertIsNone(self.verify().get("terminal"))
        self.write_terminal(self.terminal_record())
        target = self.request["terminal"]
        for malformed in [None, [], {}, {"session": target["session"]}, dict(target, turn="unknown")]:
            with self.subTest(target=malformed):
                self.request["terminal"] = malformed
                self.assertIsNone(self.verify().get("terminal"))
        self.request["terminal"] = target

    def test_wsl_hook_terminal_rejects_other_turns_and_every_later_lifecycle_boundary(self):
        self.terminal_rollout()
        terminal = self.terminal_record()
        for field in ["turn_id", "root_turn_id"]:
            changed = self.terminal_record()
            changed["payload"][field] = "01a14552-0000-7000-8000-000000000000"
            self.write_terminal(changed)
            self.assertIsNone(self.verify().get("terminal"))
        for kind in ["task_started", "task_complete", "turn_aborted"]:
            changed = self.terminal_record(kind)
            changed["payload"]["turn_id"] = "01a14552-0000-7000-8000-000000000000"
            self.write_terminal(terminal, changed)
            self.assertIsNone(self.verify().get("terminal"))
        self.write_terminal(terminal, self.terminal_record("task_started"))
        self.assertIsNone(self.verify().get("terminal"))

    def test_wsl_hook_terminal_partial_or_malformed_tail_never_reuses_earlier_completion(self):
        self.terminal_rollout()
        complete = self.terminal_record()
        for suffix in ["", "\n{", '\n{"type":"event_msg","payload":{"type":"task_started"']:
            with self.subTest(suffix=suffix):
                self.write_terminal(complete, suffix=suffix)
                self.assertIsNone(self.verify().get("terminal"))
        for record in [[], {"type": "event_msg", "payload": []}, {"type": "event_msg", "payload": None}]:
            self.write_terminal(complete, record)
            self.assertIsNone(self.verify().get("terminal"))
        padding = dict(type="response_item", payload=dict(type="message", text="x" * 4096))
        self.write_terminal(complete, *([padding] * 20))
        self.assertIsNone(self.verify().get("terminal"), "old terminal record outside bounded tail")

    def test_wsl_hook_terminal_requires_unique_owned_fd_and_bounded_main_metadata(self):
        self.terminal_rollout()
        complete = self.terminal_record()
        for field, invalid in [("id", "01a14552-0000-7000-8000-000000000000"),
                               ("source", "vscode"), ("thread_source", "guardian")]:
            saved = self.meta["payload"][field]
            self.meta["payload"][field] = invalid
            self.write_terminal(complete)
            self.assertIsNone(self.verify().get("terminal"))
            self.meta["payload"][field] = saved
        self.meta["payload"]["base_instructions"]["text"] = "x" * 65536
        self.write_terminal(complete)
        self.assertIsNone(self.verify().get("terminal"))
        self.meta["payload"]["base_instructions"]["text"] = "x" * 24000
        self.write_terminal(complete)
        (self.fd / "8").symlink_to(self.rollout)
        self.assertIsNone(self.verify().get("terminal"))

    def test_wsl_hook_terminal_rejects_rollout_uid_mismatch(self):
        self.terminal_rollout()
        self.write_terminal(self.terminal_record())
        self.request["anchor"] = None
        self.process(10, 1, 100, "bash", uid=os.getuid() + 1)
        self.process(20, 10, 200, "codex", uid=os.getuid() + 1)
        self.assertIsNone(self.verify().get("terminal"))

    def test_wsl_hook_terminal_rejects_append_or_rewrite_during_read(self):
        self.terminal_rollout()
        complete = self.terminal_record()
        real_fstat = os.fstat
        for change in ["append", "rewrite"]:
            with self.subTest(change=change):
                self.write_terminal(complete)
                calls = []

                def changing_fstat(descriptor):
                    calls.append(descriptor)
                    if len(calls) == 2:
                        if change == "append":
                            with self.rollout.open("a") as stream:
                                stream.write(json.dumps(self.terminal_record("task_started")) + "\n")
                        else:
                            info = real_fstat(descriptor)
                            os.utime(self.rollout, ns=(info.st_atime_ns, info.st_mtime_ns + 1000000))
                    return real_fstat(descriptor)

                with mock.patch.object(os, "fstat", side_effect=changing_fstat):
                    self.assertIsNone(self.verify().get("terminal"))
                self.assertGreaterEqual(len(calls), 2, "read must be followed by descriptor revalidation")

    def test_wsl_hook_terminal_revalidates_owner_after_rollout_read(self):
        self.terminal_rollout()
        self.write_terminal(self.terminal_record())
        real_fstat = os.fstat
        calls = []

        def replacing_fstat(descriptor):
            calls.append(descriptor)
            if len(calls) == 2:
                self.process(20, 10, 201, "codex", uid=os.getuid())
            return real_fstat(descriptor)

        with mock.patch.object(os, "fstat", side_effect=replacing_fstat):
            self.assertIsNone(self.verify(), "same PID with new start time cannot return evidence")

    def verify_after_fd_change(self, change):
        real_fstat = os.fstat
        calls = []

        def changing_fstat(descriptor):
            calls.append(descriptor)
            if len(calls) == 2:
                change()
            return real_fstat(descriptor)

        with mock.patch.object(os, "fstat", side_effect=changing_fstat):
            result = self.verify()
        self.assertGreaterEqual(len(calls), 2)
        return result.get("terminal")

    def test_wsl_hook_terminal_rejects_owner_fd_closed_after_bytes_are_read(self):
        self.terminal_rollout()
        self.write_terminal(self.terminal_record())
        self.assertIsNone(self.verify_after_fd_change(lambda: (self.fd / "7").unlink()))

    def test_wsl_hook_terminal_rejects_owner_fd_redirected_to_same_named_rollout(self):
        self.terminal_rollout()
        self.write_terminal(self.terminal_record())
        replacement_dir = self.root / "replacement"
        replacement_dir.mkdir()
        replacement = replacement_dir / self.rollout.name
        replacement.write_bytes(self.rollout.read_bytes())
        self.assertNotEqual(replacement.stat().st_ino, self.rollout.stat().st_ino)

        def redirect():
            (self.fd / "7").unlink()
            (self.fd / "7").symlink_to(replacement)

        self.assertIsNone(self.verify_after_fd_change(redirect))

    def test_wsl_hook_terminal_rejects_owner_fd_duplicated_during_read(self):
        self.terminal_rollout()
        self.write_terminal(self.terminal_record())

        def duplicate():
            (self.fd / "8").symlink_to(self.rollout)
            self.assertEqual((self.fd / "7").stat().st_ino, (self.fd / "8").stat().st_ino)

        self.assertIsNone(self.verify_after_fd_change(duplicate), "dup handles remain ambiguous")
