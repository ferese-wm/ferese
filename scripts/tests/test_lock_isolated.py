"""Check that nested Ferese refuses secure locking without claiming readiness.

Run with FERESE_TEST_LOCK=1 and built binaries selected by FERESE_TEST_BIN_DIR.
No credentials are submitted and the host session is never locked.
"""
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

from session_socket import ipc_environment, ipc_socket


@unittest.skipUnless(os.environ.get("FERESE_TEST_LOCK") == "1", "nested lock check is opt-in")
class NestedLockPolicyTest(unittest.TestCase):
    def test_nested_lock_refusal_preserves_unlocked_capture(self):
        repo = Path(__file__).resolve().parents[2]
        binary_dir = Path(os.environ.get("FERESE_TEST_BIN_DIR", "target/release"))
        if not binary_dir.is_absolute():
            binary_dir = repo / binary_dir
        host_display = Path(os.environ["WAYLAND_DISPLAY"])
        if not host_display.is_absolute():
            host_display = Path(os.environ["XDG_RUNTIME_DIR"]) / host_display

        with tempfile.TemporaryDirectory(prefix="ferese-lock-test-") as directory:
            root = Path(directory)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            (config / "config.kdl").write_text("")
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       XDG_CACHE_HOME=str(root / "cache"), WAYLAND_DISPLAY=str(host_display),
                       FERESE_ENABLE_SCREENCOPY="1")
            for name in ("WAYLAND_SOCKET", "FERESE_SHELL_CONTROL_SOCKET"):
                env.pop(name, None)
            env = ipc_environment(env)
            with (root / "compositor.log").open("w+") as log:
                compositor = subprocess.Popen(
                    [str(binary_dir / "ferese"), "--backend", "nested"],
                    env=env, stdout=log, stderr=subprocess.STDOUT,
                )
                try:
                    deadline = time.monotonic() + 15
                    while not ipc_socket(runtime).exists():
                        if compositor.poll() is not None or time.monotonic() >= deadline:
                            log.seek(0)
                            self.fail(log.read())
                        time.sleep(.05)
                    sockets = [path for path in runtime.glob("wayland-*")
                               if not path.name.endswith(".lock")]
                    self.assertEqual(len(sockets), 1)
                    env.update(WAYLAND_DISPLAY=str(sockets[0]), FERESE_LOCK_READY="1")
                    env = ipc_environment(env)
                    for arguments in ([], ["--foreground"]):
                        result = subprocess.run(
                            [str(binary_dir / "ferese-lock"), *arguments], env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=20,
                        )
                        self.assertNotEqual(result.returncode, 0, result.stderr)
                        self.assertNotIn("FERESE_LOCKED", result.stdout)
                        self.assertIn("does not support secure session locking", result.stderr)

                    result = subprocess.run(
                        [str(binary_dir / "feresectl"), "screenshot"], env=env,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr.decode())
                    self.assertTrue(result.stdout.startswith(b"\x89PNG\r\n\x1a\n"))
                finally:
                    compositor.terminate()
                    try:
                        compositor.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        compositor.kill()
                        compositor.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
