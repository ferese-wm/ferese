"""Opt-in real broker/CLI checks on an isolated software-rendered desktop."""
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest
from session_socket import ipc_socket, ipc_environment

ROOT = Path(__file__).resolve().parents[2]

@unittest.skipUnless(os.environ.get("FERESE_TEST_ACTIVITIES") == "1", "opt-in isolated activities test")
class Activities(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix="ferese-activities-")
        cls.root = Path(cls.temporary.name)
        cls.runtime = cls.root / "runtime"
        cls.runtime.mkdir(mode=0o700)
        cls.config = cls.root / "config/ferese"
        cls.config.mkdir(parents=True)
        (cls.config / "config.kdl").write_text('panel main { style morph; end { group tasks { item activities kind=activities; }; }; }\nactivities { sources { shell-hook #true; }; }\n')
        cls.env = dict(os.environ, XDG_RUNTIME_DIR=str(cls.runtime), XDG_CONFIG_HOME=str(cls.root / "config"), XDG_STATE_HOME=str(cls.root / "state"), XDG_CACHE_HOME=str(cls.root / "cache"), LIBGL_ALWAYS_SOFTWARE="1")
        cls.env.pop("FERESE_SOCKET", None)
        cls.env.pop("WAYLAND_SOCKET", None)
        cls.log = (cls.root / "native.log").open("w+")
        cls.children = []
        cls.ctl = ROOT / os.environ.get("FERESE_TEST_CTL", "target/debug/feresectl")
        cls.launch(["weston", "--backend=headless-backend.so", "--renderer=pixman", "--shell=kiosk-shell.so", "--socket=activity-host", "--idle-time=0", "--no-config"])
        cls.wait(lambda: (cls.runtime / "activity-host").is_socket())
        cls.env["WAYLAND_DISPLAY"] = "activity-host"
        binary = ROOT / os.environ.get("FERESE_TEST_BINARY", "target/debug/ferese")
        cls.launch([str(binary), "--backend", "nested"])
        cls.wait(lambda: ipc_socket(cls.runtime).is_socket())
        cls.env = ipc_environment(cls.env)
        cls.wait(lambda: cls.query("outputs"))

    @classmethod
    def launch(cls, args, **kwargs):
        child = subprocess.Popen(args, env=cls.env, stdout=cls.log, stderr=cls.log, start_new_session=True, **kwargs)
        cls.children.append(child)
        return child

    @classmethod
    def wait(cls, predicate, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(.025)
        cls.log.flush()
        raise AssertionError((cls.root / "native.log").read_text()[-8000:])

    @classmethod
    def call(cls, *args, check=True):
        result = subprocess.run([str(cls.ctl), *args], env=cls.env, text=True, capture_output=True, timeout=5)
        if check and result.returncode:
            raise AssertionError(result.stderr)
        return result

    @classmethod
    def query(cls, *args):
        result = cls.call("--json", *args, check=False)
        return json.loads(result.stdout) if result.returncode == 0 else None

    @classmethod
    def tearDownClass(cls):
        for child in reversed(cls.children):
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGINT)
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()
        cls.log.close()
        cls.temporary.cleanup()

    def test_owner_scope_quota_terminal_stale_handles_and_atomic_reload(self):
        for local in ("one", "two", "three"):
            self.call("activity", "start", local, "--title", "Build", "--progress", "0.2")
        failed = self.call("activity", "start", "four", "--title", "Build", check=False)
        self.assertNotEqual(failed.returncode, 0)
        snapshot = self.query("activity", "list")
        self.assertEqual(len(snapshot["activities"]), 3)
        handle = next(entry["id"] for entry in snapshot["activities"] if entry["id"]["local"] == "one")
        other = subprocess.run(["sh", "-c", '"$1" activity update one --progress 0.8', "sh", str(self.ctl)], env=self.env, capture_output=True)
        self.assertNotEqual(other.returncode, 0, "another caller must not update this owner's ID")
        stale = dict(handle, generation=handle["generation"] + 10)
        self.assertNotEqual(self.call("activity", "update", "one", "--handle", json.dumps(stale), "--progress", "0.8", check=False).returncode, 0)
        self.call("activity", "cancel", "two")
        self.call("activity", "cancel", "three")
        time.sleep(.35)
        self.call("activity", "end", "one", "--result", "ok")
        entry = self.query("activity", "list")["activities"][0]
        deadline = entry["result_deadline"]
        self.call("activity", "end", "one", "--result", "ok")
        self.assertEqual(self.query("activity", "list")["activities"][0]["result_deadline"], deadline)
        (self.config / "config.kdl").write_text("activities { max-visible 9; }\n")
        self.assertNotEqual(self.call("reload-config", check=False).returncode, 0)
        self.assertEqual(self.query("activity", "list")["limits"]["max_visible"], 2)
        self.wait(lambda: not self.query("activity", "list")["activities"])

    def test_hook_nonmatches_are_discarded_and_owner_death_is_not_completion(self):
        self.call("activity", "hook", "nonmatch", "--command", "private unmatched value", "--cwd", "/private/cwd")
        self.assertNotIn("private", json.dumps(self.query("activity", "list")))
        wrapper = subprocess.Popen([str(self.ctl), "run", "--title", "Observed", "--", "sleep", "30"], env=self.env, start_new_session=True)
        try:
            self.wait(lambda: any(entry["data"]["title"] == "Observed" for entry in self.query("activity", "list")["activities"]))
            os.killpg(wrapper.pid, signal.SIGKILL)
            wrapper.wait()
            self.wait(lambda: not any(entry["data"]["title"] == "Observed" for entry in self.query("activity", "list")["activities"]))
        finally:
            if wrapper.poll() is None:
                os.killpg(wrapper.pid, signal.SIGKILL)
                wrapper.wait()

if __name__ == "__main__":
    unittest.main()
