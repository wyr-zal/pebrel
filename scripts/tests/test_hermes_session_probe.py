"""Exercise the production probe with isolated native metadata and a real Linux PTY."""
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[2] / "nebula_app/res/hooks/hermes_session_probe.py"
SPEC = importlib.util.spec_from_file_location("hermes_probe", SOURCE)
PROBE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBE)


class HermesSessionProbeTests(unittest.TestCase):
    ID = "20261107_091523_a1b2c3"
    OTHER = "20261107_101112_b1c2d3"

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hermes-identity-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / "hermes"
        (self.home / "terminal-sessions").mkdir(parents=True)
        (self.home / "runtime").mkdir()
        self.proc = self.root / "proc"
        self.owner = self.proc / "789"
        (self.owner / "fd").mkdir(parents=True)
        self.master, self.slave = os.openpty()
        self.addCleanup(os.close, self.master)
        self.addCleanup(os.close, self.slave)
        self.tty = os.ttyname(self.slave)
        self.fields = ["0"] * 24
        self.fields[:6] = ["S", "1", "789", "789", str(os.stat(self.tty).st_rdev), "789"]
        self.fields[19] = "100"
        self.write_stat()
        boot = int(time.time()) - 10
        (self.proc / "stat").write_text(f"btime {boot}\n")
        self.started = boot + 100 / os.sysconf("SC_CLK_TCK")
        self.environment = {
            "PEBREL_PANE_ID": "42", "PEBREL_PROCESS_ID": "fixture-instance",
            "PEBREL_COMMAND_ID": "fixture-command", "HERMES_HOME": str(self.home),
        }
        self.write_environment()
        (self.owner / "cmdline").write_bytes(b"python3\0/fixture/hermes\0")
        (self.owner / "cwd").symlink_to("/guest/current")
        for descriptor in ("0", "1"):
            (self.owner / "fd" / descriptor).symlink_to(self.tty)
        database = self.home / "state.db"
        with sqlite3.connect(database) as connection:
            connection.execute("CREATE TABLE sessions (id TEXT PRIMARY KEY)")
            connection.executemany("INSERT INTO sessions VALUES (?)", [(self.ID,), (self.OTHER,)])
        (self.owner / "fd/3").symlink_to(database)
        self.crumb = self.home / "terminal-sessions" / ("tty-" + self.tty.strip("/").replace("/", "-"))
        self.write_crumb(self.ID)

    def write_stat(self):
        (self.owner / "stat").write_text("789 (hermes) " + " ".join(self.fields))

    def write_environment(self):
        (self.owner / "environ").write_bytes(b"\0".join(f"{k}={v}".encode() for k, v in self.environment.items()) + b"\0")

    def write_crumb(self, session_id, stamp=None):
        self.crumb.write_text(json.dumps({"session_id": session_id, "cwd": "/unrelated/old", "ts": time.time() if stamp is None else stamp}))

    def query(self):
        return PROBE.probe(self.proc, "42", "fixture-instance", "789", "100", "fixture-command")

    def write_lease(self, session_id, created=None):
        record = {"pid": 789, "process_start_time": self.started if created is None else created,
                  "surface": "cli", "session_id": session_id}
        (self.home / "runtime/active_sessions.json").write_text(json.dumps({"entries": [record]}))

    def test_exact_terminal_identity_wins_over_a_newer_sibling_and_cwd(self):
        result = self.query()
        self.assertEqual(result, {"session_id": self.ID, "cwd": "/guest/current"})

    def test_pane_instance_command_and_pid_birth_must_match(self):
        for key in ("PEBREL_PANE_ID", "PEBREL_PROCESS_ID", "PEBREL_COMMAND_ID"):
            with self.subTest(key=key):
                old = self.environment[key]
                self.environment[key] = "foreign"
                self.write_environment()
                self.assertIsNone(self.query())
                self.environment[key] = old
        self.write_environment()
        self.fields[19] = "99"
        self.write_stat()
        self.assertIsNone(self.query())

    def test_recycled_tty_and_future_breadcrumbs_are_rejected(self):
        for stamp in (self.started - 1, time.time() + 60, float("nan")):
            with self.subTest(stamp=stamp):
                self.write_crumb(self.OTHER, stamp)
                self.assertIsNone(self.query())

    def test_pipe_frontend_requires_a_matching_native_process_lease(self):
        self.crumb.write_text("{}")
        self.write_lease(self.ID)
        self.assertEqual(self.query()["session_id"], self.ID)
        for created in (self.started - 1, float("nan"), True):
            self.write_lease(self.OTHER, created)
            self.assertIsNone(self.query())

    def test_conflicting_native_identities_are_not_guessed(self):
        self.write_lease(self.OTHER)
        self.assertIsNone(self.query())

    def test_deleted_session_is_not_replaced_by_the_other_db_row(self):
        self.write_crumb("20261107_000000_deadbe")
        self.assertIsNone(self.query())

    def test_profile_is_read_from_the_open_database_instead_of_the_default_home(self):
        self.environment["HERMES_HOME"] = str(self.root / "wrong-home")
        self.write_environment()
        (self.owner / "cmdline").write_bytes(b"python3\0/fixture/hermes\0--profile\0fixture\0")
        self.assertEqual(self.query()["session_id"], self.ID)

    def test_running_cpu_counters_do_not_retire_the_same_process(self):
        original = PROBE.stat_fields
        calls = 0

        def advancing(directory):
            nonlocal calls
            values = original(directory)
            calls += 1
            if calls > 1:
                values[11] = "9000"
                values[0] = "R"
            return values

        with patch.object(PROBE, "stat_fields", advancing):
            self.assertEqual(self.query()["session_id"], self.ID)

    def test_real_linux_process_and_pty_with_a_full_local_id(self):
        local_id = "local-" + "a" * 64
        with sqlite3.connect(self.home / "state.db") as connection:
            connection.execute("INSERT INTO sessions VALUES (?)", (local_id,))
        helper = self.root / "bin" / "hermes"
        helper.parent.mkdir()
        helper.write_text(f'''
import fcntl, importlib.util, json, os, sqlite3, termios, time
from pathlib import Path
fcntl.ioctl(0, termios.TIOCSCTTY, 0)
home = Path(os.environ["HERMES_HOME"])
connection = sqlite3.connect(home / "state.db")
tty = os.ttyname(0)
(home / "terminal-sessions" / ("tty-" + tty.strip("/").replace("/", "-"))).write_text(json.dumps({{"session_id": {local_id!r}, "ts": time.time()}}))
spec = importlib.util.spec_from_file_location("probe", {str(SOURCE)!r})
probe = importlib.util.module_from_spec(spec); spec.loader.exec_module(probe)
start = probe.stat_fields(Path("/proc") / str(os.getpid()))[19]
print(json.dumps(probe.probe(Path("/proc"), "42", "fixture-instance", str(os.getpid()), start, "fixture-command")))
''')
        environment = {**os.environ, **self.environment}
        result = subprocess.run([sys.executable, str(helper)], stdin=self.slave, capture_output=True,
                                text=True, env=environment, start_new_session=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["session_id"], local_id)


if __name__ == "__main__":
    unittest.main()
