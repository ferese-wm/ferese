"""Verify real on-demand/eager XWayland startup and teardown in a nested session.

FERESE_TEST_XWAYLAND=1 python3 scripts/tests/test_xwayland_isolated.py
Requires a Wayland host, Xwayland, xdpyinfo and Satellite built with systemd support.
"""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

from session_socket import ipc_environment, ipc_socket


@unittest.skipUnless(os.environ.get("FERESE_TEST_XWAYLAND") == "1", "opt-in nested XWayland test")
class XwaylandTest(unittest.TestCase):
    def test_startup_authorization_and_owned_process_cleanup(self):
        repo = Path(__file__).resolve().parents[2]
        binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/release/ferese")
        ctl = repo / os.environ.get("FERESE_TEST_CTL", "target/release/feresectl")
        satellite = os.environ.get("FERESE_TEST_SATELLITE") or shutil.which("xwayland-satellite")
        self.assertIsNotNone(satellite, "Satellite must be installed; build it with --features systemd")
        satellite = str(Path(satellite).resolve())
        self.assertTrue(Path(satellite).is_file(), f"Satellite does not exist: {satellite}")
        for mode in ("on-demand", "eager"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory(prefix="ferese-xwayland-") as temporary:
                root = Path(temporary)
                runtime = root / "runtime"
                runtime.mkdir(mode=0o700)
                config = root / "config/ferese"
                config.mkdir(parents=True)
                (config / "config.kdl").write_text(
                    f'xwayland {{ startup "{mode}"; path {json.dumps(satellite)}; }}\n')
                display = Path(os.environ["WAYLAND_DISPLAY"])
                if not display.is_absolute():
                    display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
                env = {key: value for key, value in os.environ.items()
                       if not key.startswith("FERESE_") or key.startswith("FERESE_TEST_")}
                for key in ("WAYLAND_SOCKET", "DISPLAY", "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS"):
                    env.pop(key, None)
                env.update(XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=str(display),
                           XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                           XDG_CACHE_HOME=str(root / "cache"))
                # This managed child receives Ferese's reserved display and cookie.
                # Waiting for a gate lets us prove on-demand stays idle until an X client connects.
                client = root / "client.py"
                client.write_text('''import json, os
from pathlib import Path
import subprocess, time
root = Path(__file__).parent
def publish(name, value):
    temporary = root / (name + ".tmp")
    temporary.write_text(json.dumps(value))
    temporary.replace(root / name)
publish("endpoint.json", {
    "display": os.environ["DISPLAY"], "authority": os.environ["XAUTHORITY"]})
deadline = time.monotonic() + 30
while not (root / "connect").exists():
    if time.monotonic() >= deadline:
        raise SystemExit("X client was never released")
    time.sleep(.025)
result = subprocess.run(["xdpyinfo"], capture_output=True, text=True, timeout=15)
publish("client-result.json", {
    "code": result.returncode, "stdout": result.stdout, "stderr": result.stderr})
''')
                log_path = root / "compositor.log"
                with log_path.open("w") as log:
                    compositor = subprocess.Popen([
                        str(binary), "--backend", "nested", "--", sys.executable, str(client),
                    ], env=ipc_environment(env), stdout=log, stderr=log, start_new_session=True)

                    def wait_for(predicate, timeout=15):
                        deadline = time.monotonic() + timeout
                        while time.monotonic() < deadline:
                            value = predicate()
                            if value:
                                return value
                            if compositor.poll() is not None:
                                break
                            time.sleep(.025)
                        self.fail(log_path.read_text()[-12000:])

                    def command(*args):
                        return subprocess.run([str(ctl), "-j", *args], env=ipc_environment(env),
                                              check=True, capture_output=True, text=True, timeout=5)

                    def status_is(state):
                        status = json.loads(command("xwayland", "status").stdout)
                        self.assertNotEqual(status["state"], "failed", status["last_error"])
                        return status if status["state"] == state else None

                    try:
                        wait_for(lambda: ipc_socket(runtime).is_socket())
                        initial = wait_for(lambda: status_is("idle" if mode == "on-demand" else "running"))
                        self.assertEqual(initial["effective_startup"], mode)
                        wait_for(lambda: (root / "endpoint.json").exists())
                        endpoint = json.loads((root / "endpoint.json").read_text())
                        self.assertEqual(endpoint["display"], initial["display"])
                        self.assertTrue(Path(endpoint["authority"]).is_relative_to(runtime))
                        (root / "connect").touch()
                        running = wait_for(lambda: status_is("running"), timeout=10)
                        self.assertEqual(running["readiness"], "verified")
                        self.assertEqual(running["recent_failures"], 0)
                        wait_for(lambda: (root / "client-result.json").exists())
                        result = json.loads((root / "client-result.json").read_text())
                        self.assertEqual(result["code"], 0, result["stderr"])
                        self.assertIn("X.Org", result["stdout"])
                        # Record the complete owned service group before requesting a clean exit.
                        leader = running["satellite_pid"]
                        members = []
                        for proc in Path("/proc").iterdir():
                            if not proc.name.isdigit():
                                continue
                            try:
                                stat = (proc / "stat").read_text()
                                if int(stat[stat.rfind(")") + 1:].split()[2]) == leader:
                                    members.append(proc)
                            except (FileNotFoundError, ProcessLookupError, PermissionError):
                                continue
                        self.assertGreaterEqual(len(members), 2, "Satellite and Xwayland must both be running")
                        command("exit")
                        compositor.wait(timeout=10)
                        self.assertEqual(compositor.returncode, 0, log_path.read_text())
                        self.assertFalse(any(proc.exists() for proc in members), "owned X11 processes survived shutdown")
                        self.assertFalse(Path(endpoint["authority"]).exists(), "Xauthority survived shutdown")
                        slot = endpoint["display"].removeprefix(":")
                        self.assertFalse(Path(f"/tmp/.X11-unix/X{slot}").exists(), "X11 socket survived shutdown")
                        self.assertFalse(Path(f"/tmp/.X{slot}-lock").exists(), "X11 display lock survived shutdown")
                        self.assertNotIn("panicked at", log_path.read_text())
                        print(f"XWayland: {mode} startup, authorized client and complete teardown passed", flush=True)
                    finally:
                        if compositor.poll() is None:
                            os.killpg(compositor.pid, signal.SIGTERM)
                            try:
                                compositor.wait(timeout=10)
                            except subprocess.TimeoutExpired:
                                os.killpg(compositor.pid, signal.SIGKILL)
                                compositor.wait(timeout=5)
                        if directory := os.environ.get("FERESE_TEST_LOG_DIR"):
                            logs = Path(directory)
                            logs.mkdir(parents=True, exist_ok=True)
                            shutil.copyfile(log_path, logs / f"xwayland-{mode}.log")


if __name__ == "__main__":
    unittest.main()
