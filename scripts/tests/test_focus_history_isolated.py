"""Focus history against real windows on a disposable nested compositor.

FERESE_TEST_FOCUS_HISTORY=1 python3 scripts/tests/test_focus_history_isolated.py
Requires built debug ferese/feresectl, foot, dbus-daemon, and a Wayland host.
"""
import json
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import signal
import subprocess
import tempfile
import time
import unittest


@unittest.skipUnless(os.environ.get("FERESE_TEST_FOCUS_HISTORY") == "1", "nested focus test is opt-in")
class FocusHistoryTest(unittest.TestCase):
    def test_last_focus_crosses_workspaces_and_skips_closed_windows(self):
        repo = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="ferese-focus-test-") as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            (config / "config.kdl").write_text("animations { enabled #false; }\n")
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
            env = dict(os.environ, WAYLAND_DISPLAY=str(display), XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"))
            env.pop("WAYLAND_SOCKET", None)
            env.pop("FERESE_SHELL_CONTROL_SOCKET", None)
            children = []

            def launch(command, **kwargs):
                child = subprocess.Popen(command, env=ipc_environment(env), start_new_session=True, **kwargs)
                children.append(child)
                return child

            def command(*args):
                return json.loads(subprocess.check_output(
                    [str(repo / "target/debug/feresectl"), "-j", *args], env=ipc_environment(env), timeout=5
                ))

            def wait_for(predicate):
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    result = predicate()
                    if result:
                        return result
                    time.sleep(.025)
                self.fail("Timed out waiting for compositor state: " + (root / "compositor.log").read_text())

            def focused():
                result = command("get-focused-window")
                return result["id"] if result else None

            with (root / "compositor.log").open("w") as log:
                try:
                    bus = launch(["dbus-daemon", "--session", "--nofork", "--print-address=1"],
                                 stdout=subprocess.PIPE, text=True)
                    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                    launch([str(repo / "target/debug/ferese"), "--backend", "nested"], stdout=log, stderr=log)
                    wait_for(lambda: (ipc_socket(runtime)).is_socket())
                    command("focus-last-window")
                    self.assertIsNone(focused())
                    env["WAYLAND_DISPLAY"] = str(next(
                        path for path in runtime.glob("wayland-*") if not path.name.endswith(".lock")
                    ))
                    windows = []
                    for index in range(3):
                        app = f"ferese.test.focus.{index}"
                        child = launch(["foot", "--app-id", app, "sleep", "120"], stdout=log, stderr=log)
                        window = wait_for(lambda: next((w for w in command("get-windows")
                                                       if w["app_id"] == app and w["width"] > 0), None))
                        wait_for(lambda: focused() == window["id"])
                        windows.append((child, window["id"]))
                        if index == 1:
                            command("workspace", "2")
                    first, second, third = [window for _, window in windows]
                    command("focus-last-window")
                    self.assertEqual(focused(), second)
                    self.assertEqual(next(w["name"] for w in command("get-workspaces") if w["active"]), "1")
                    command("focus-last-window")
                    self.assertEqual(focused(), third)
                    self.assertEqual(next(w["name"] for w in command("get-workspaces") if w["active"]), "2")
                    command("focus-mru-previous")
                    self.assertEqual(focused(), first, "reverse traversal reaches least recent window")
                    command("focus-mru-next")
                    self.assertEqual(focused(), third)
                    windows[1][0].terminate()
                    windows[1][0].wait(timeout=3)
                    wait_for(lambda: not any(w["id"] == second for w in command("get-windows")))
                    command("focus-last-window")
                    self.assertEqual(focused(), first, "closed windows must not consume a focus action")
                    command("reload-config")
                    command("focus-last-window")
                    self.assertEqual(focused(), third, "reload must preserve focus history")
                finally:
                    for child in reversed(children):
                        if child.poll() is None:
                            os.killpg(child.pid, signal.SIGTERM)
                            try:
                                child.wait(timeout=3)
                            except subprocess.TimeoutExpired:
                                os.killpg(child.pid, signal.SIGKILL)
                                child.wait(timeout=3)
                        if child.stdout is not None:
                            child.stdout.close()


if __name__ == "__main__":
    unittest.main()
