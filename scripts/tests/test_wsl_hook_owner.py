"""Kernel evidence contract for the dedicated WSL hook channel."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest


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
