"""Opt-in real broker/CLI checks on an isolated software-rendered desktop."""
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import struct
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
        host = os.environ.get("WAYLAND_DISPLAY")
        if host and not Path(host).is_absolute():
            host = str(Path(os.environ["XDG_RUNTIME_DIR"]) / host)
        cls.temporary = tempfile.TemporaryDirectory(prefix="ferese-activities-")
        cls.root = Path(cls.temporary.name)
        cls.runtime = cls.root / "runtime"
        cls.runtime.mkdir(mode=0o700)
        cls.config = cls.root / "config/ferese"
        cls.config.mkdir(parents=True)
        (cls.config / "config.kdl").write_text('status { keybinding-guide #false; }\npanel main { style morph; end { group tasks { item activities kind=activities; }; }; }\nactivities { sources { shell-hook #true; }; }\n')
        cls.env = dict(os.environ, XDG_RUNTIME_DIR=str(cls.runtime), XDG_CONFIG_HOME=str(cls.root / "config"), XDG_STATE_HOME=str(cls.root / "state"), XDG_CACHE_HOME=str(cls.root / "cache"), LIBGL_ALWAYS_SOFTWARE="1")
        cls.env.pop("FERESE_SOCKET", None)
        cls.env.pop("WAYLAND_SOCKET", None)
        for key in ("FERESE_SHELL_CONTROL_SOCKET", "DISPLAY", "XAUTHORITY", "DBUS_SESSION_BUS_PID"):
            cls.env.pop(key, None)
        cls.log = (cls.root / "native.log").open("w+")
        cls.children = []
        cls.addClassCleanup(cls.cleanup)
        cls.ctl = ROOT / os.environ.get("FERESE_TEST_CTL", "target/debug/feresectl")
        if not host:
            cls.launch([os.environ.get("FERESE_TEST_WESTON", "weston"), "--backend=headless-backend.so", "--renderer=pixman", "--shell=kiosk-shell.so", "--socket=activity-host", "--idle-time=0", "--no-config"])
            cls.wait(lambda: (cls.runtime / "activity-host").is_socket())
            host = str(cls.runtime / "activity-host")
        cls.env["WAYLAND_DISPLAY"] = host
        bus_config = cls.root / "bus.conf"
        bus_config.write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
                             '<policy context="default"><allow send_destination="*"/>'
                             '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        bus = subprocess.Popen(["dbus-daemon", "--config-file=" + str(bus_config), "--nofork", "--print-address=1"],
                               env=cls.env, stdout=subprocess.PIPE, stderr=cls.log, text=True, start_new_session=True)
        cls.children.append(bus)
        cls.env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
        bus.stdout.close()
        binary = ROOT / os.environ.get("FERESE_TEST_BINARY", "target/debug/ferese")
        shell = ROOT / os.environ.get("FERESE_TEST_SHELL", "target/debug/ferese-shell")
        cls.launch([str(binary), "--backend", "nested", "--grant-shell-control", "--grant-effects", "--",
                    "env", "ICED_BACKEND=tiny-skia", "WAYLAND_DEBUG=client", str(shell)])
        cls.wait(lambda: ipc_socket(cls.runtime).is_socket())
        cls.env = ipc_environment(cls.env)
        cls.wait(lambda: cls.query("outputs"))
        cls.wait(lambda: '"ferese-shell-top-bar"' in (cls.root / "native.log").read_text())

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
    def cleanup(cls):
        for child in reversed(cls.children):
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGINT)
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()
        if not cls.log.closed:
            cls.log.close()
        if (destination := os.environ.get("FERESE_TEST_LOG_DIR")) and (cls.root / "native.log").exists():
            Path(destination).mkdir(parents=True, exist_ok=True)
            shutil.copyfile(cls.root / "native.log", Path(destination) / "activities-native.log")
        cls.temporary.cleanup()

    def test_pomodoro_survives_cli_disconnect_and_controls_preserve_time(self):
        self.call("pomodoro", "stop")
        try:
            first = self.query("pomodoro", "start")
            self.assertIsNotNone(first)
            self.assertNotEqual(first["id"]["owner"]["pid"], os.getpid())
            self.assertEqual(first["timer"]["phase"], "work")
            self.assertNotEqual(self.call("pomodoro", "start", check=False).returncode, 0)
            paused = self.query("pomodoro", "pause")
            remaining = paused["timer"]["remaining_ms"]
            time.sleep(.15)
            self.assertEqual(self.query("pomodoro", "status")["timer"]["remaining_ms"], remaining)
            entries = self.query("activity", "list")["activities"]
            timer = next(entry for entry in entries if entry["id"] == first["id"])
            self.assertEqual(timer["data"]["value"]["kind"], "paused-countdown")
            self.assertIn("Paused", timer["data"]["title"])
            resumed = self.query("pomodoro", "resume")
            self.assertEqual(resumed["id"], first["id"])
            self.assertEqual(resumed["timer"]["remaining_ms"], remaining)
            skipped = self.query("pomodoro", "next")
            self.assertEqual(skipped["timer"]["phase"], "short-break")
            self.assertEqual(skipped["timer"]["completed_work"], 0)
            reset = self.query("pomodoro", "reset")
            self.assertNotEqual(reset["id"], first["id"])
            self.assertTrue(all(child.poll() is None for child in self.children))
        finally:
            self.call("pomodoro", "stop")
        self.assertIsNone(self.query("pomodoro", "status"))

    def test_live_watch_and_native_bar_render_progress_without_resizing(self):
        def bar():
            trace = (self.root / "native.log").read_text()
            matches = re.findall(r'get_layer_surface\(new id zwlr_layer_surface_v1[@#](\d+), wl_surface[@#](\d+)[^\n]*"ferese-shell-top-bar"', trace)
            if not matches:
                return None
            layer, surface = matches[-1]
            sizes = re.findall(r'zwlr_layer_surface_v1[@#]' + layer + r'\.set_size\((\d+), (\d+)\)', trace)
            frames = len(re.findall(r'wl_surface[@#]' + surface + r'\.frame\(', trace))
            return (tuple(map(int, sizes[-1])), frames) if sizes else None

        self.call("activity", "start", "render", "--title", "Native Cargo build", "--progress", "0.2")
        try:
            self.wait(lambda: any(entry["id"]["local"] == "render" for entry in self.query("activity", "list")["activities"]))
            time.sleep(1.2)  # Eligibility delay and shell morph settlement.
            before = self.wait(bar)
            snapshot = self.query("activity", "list")
            with socket.socket(socket.AF_UNIX) as watch:
                watch.settimeout(5)
                watch.connect(str(ipc_socket(self.runtime)))
                request = json.dumps({"version": 1, "id": 77, "type": "command", "command": "activity-watch", "args": {"since": snapshot["revision"]}}).encode()
                watch.sendall(struct.pack(">I", len(request)) + request)
                self.call("activity", "update", "render", "--progress", "0.8")
                def read_exact(count):
                    data = b""
                    while len(data) < count:
                        part = watch.recv(count - len(data))
                        self.assertTrue(part, "watch disconnected")
                        data += part
                    return data
                response = json.loads(read_exact(struct.unpack(">I", read_exact(4))[0]))
                self.assertEqual(response["id"], 77)
                current = next(entry for entry in response["result"]["activities"] if entry["id"]["local"] == "render")
                self.assertAlmostEqual(current["data"]["value"]["value"], .8)
            after = self.wait(lambda: (value := bar()) and value[1] > before[1] and value)
            self.assertEqual(after[0], before[0], "progress should not resize the island")
            self.assertTrue(all(child.poll() is None for child in self.children))
        finally:
            self.call("activity", "cancel", "render", check=False)

    def test_owner_scope_quota_terminal_stale_handles_and_atomic_reload(self):
        for local in ("one", "two", "three"):
            self.call("activity", "start", local, "--title", "Build", "--progress", "0.2")
        failed = self.call("activity", "start", "four", "--title", "Build", check=False)
        self.assertNotEqual(failed.returncode, 0)
        snapshot = self.query("activity", "list")
        self.assertEqual(len(snapshot["activities"]), 3)
        handle = next(entry["id"] for entry in snapshot["activities"] if entry["id"]["local"] == "one")
        other = subprocess.run(["sh", "-c", '"$1" activity update one --progress 0.8; status=$?; exit "$status"', "sh", str(self.ctl)], env=self.env, capture_output=True)
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
