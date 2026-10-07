"""Check client material lifetimes on a disposable nested compositor.

FERESE_TEST_MATERIAL=1 python3 scripts/tests/test_material_isolated.py
Requires built debug compositor binaries, a Wayland host and dbus-daemon.
"""
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import signal
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(os.environ.get("FERESE_TEST_MATERIAL") == "1", "requires a Wayland host")
class MaterialLifetime(unittest.TestCase):
    def test_attachment_and_release(self):
        with tempfile.TemporaryDirectory(prefix="ferese-material-") as directory:
            root = Path(directory)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            (config / "config.kdl").write_text("")
            host = Path(os.environ["WAYLAND_DISPLAY"])
            if not host.is_absolute():
                host = Path(os.environ["XDG_RUNTIME_DIR"]) / host
            env = dict(os.environ, WAYLAND_DISPLAY=str(host), XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
                       XDG_STATE_HOME=str(root / "state"), XDG_CACHE_HOME=str(root / "cache"))
            children = []
            with (root / "compositor.log").open("w") as log:
                try:
                    bus = subprocess.Popen(["dbus-daemon", "--session", "--nofork", "--print-address=1"],
                                           env=ipc_environment(env), stdout=subprocess.PIPE, text=True, start_new_session=True)
                    children.append(bus)
                    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                    compositor = subprocess.Popen([str(ROOT / "target/debug/ferese"), "--backend", "nested"],
                                                  env=ipc_environment(env), stdout=log, stderr=log, start_new_session=True)
                    children.append(compositor)
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        self.assertIsNone(compositor.poll(), (root / "compositor.log").read_text())
                        sockets = [p for p in runtime.glob("wayland-*") if p.is_socket()]
                        if sockets and (ipc_socket(runtime)).is_socket():
                            break
                        time.sleep(.05)
                    else:
                        self.fail("Nested compositor did not become ready")
                    env["WAYLAND_DISPLAY"] = str(sockets[0])
                    result = subprocess.run([
                        "cargo", "test", "--workspace", "--locked",
                        "material::tests::attachment_acknowledgement_and_last_clone_release",
                        "--", "--ignored", "--exact",
                    ], cwd=ROOT, env=ipc_environment(env), capture_output=True, text=True, timeout=120)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("1 passed", result.stdout)
                    self.assertIsNone(compositor.poll())
                finally:
                    for child in reversed(children):
                        if child.poll() is None:
                            os.killpg(child.pid, signal.SIGTERM)
                        child.wait(timeout=5)
                        if child.stdout:
                            child.stdout.close()


if __name__ == "__main__":
    unittest.main()
