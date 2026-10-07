"""Workspace history against a temporary nested compositor.

FERESE_TEST_WORKSPACE_HISTORY=1 python3 scripts/tests/test_workspace_history_isolated.py
Requires target/debug/ferese, feresectl, foot, dbus-daemon, and a Wayland host.
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


@unittest.skipUnless(os.environ.get("FERESE_TEST_WORKSPACE_HISTORY") == "1", "nested workspace test is opt-in")
class WorkspaceHistoryTest(unittest.TestCase):
    def test_dynamic_positions_spare_cleanup_and_history(self):
        repo = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="ferese-workspace-test-") as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            source = config / "config.kdl"
            source.write_text("animations { enabled #false; }\n")
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
            env = dict(os.environ, WAYLAND_DISPLAY=str(display), XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / "config"))
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

            def active():
                return next(workspace["name"] for workspace in command("get-workspaces") if workspace["active"])

            with (root / "compositor.log").open("w") as log:
                try:
                    bus = launch(["dbus-daemon", "--session", "--nofork", "--print-address=1"],
                                 stdout=subprocess.PIPE, text=True)
                    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                    compositor = launch([str(repo / "target/debug/ferese"), "--backend", "nested"],
                                        stdout=log, stderr=log)
                    deadline = time.monotonic() + 10
                    while not (ipc_socket(runtime)).is_socket():
                        if compositor.poll() is not None or time.monotonic() >= deadline:
                            self.fail((root / "compositor.log").read_text())
                        time.sleep(.025)
                    def wait_for(predicate):
                        deadline = time.monotonic() + 10
                        while time.monotonic() < deadline:
                            value = predicate()
                            if value:
                                return value
                            time.sleep(.025)
                        self.fail("Timed out: " + (root / "compositor.log").read_text())

                    def workspaces():
                        return command("get-workspaces")

                    initial = workspaces()[0]["id"]
                    self.assertEqual(len(workspaces()), 1)
                    command("workspace", "9")
                    self.assertEqual(workspaces()[0]["id"], initial)
                    self.assertEqual(len(workspaces()), 1, "numeric lookup must not create gaps")
                    env["WAYLAND_DISPLAY"] = str(next(
                        path for path in runtime.glob("wayland-*") if not path.name.endswith(".lock")
                    ))

                    first = launch(["foot", "--app-id", "ferese.test.workspace.first", "sleep", "120"],
                                   stdout=log, stderr=log)
                    wait_for(lambda: len(workspaces()) == 2 and workspaces()[0]["window_count"] == 1)
                    command("workspace", "2")
                    second_id = next(w["id"] for w in workspaces() if w["focused"])
                    second = launch(["foot", "--app-id", "ferese.test.workspace.second", "sleep", "120"],
                                    stdout=log, stderr=log)
                    wait_for(lambda: len(workspaces()) == 3 and workspaces()[1]["window_count"] == 1)
                    for expected in ["1", "2", "1", "2"]:
                        command("workspace-back-and-forth")
                        self.assertEqual(active(), expected)

                    os.killpg(first.pid, signal.SIGTERM)
                    first.wait(timeout=3)
                    wait_for(lambda: len(workspaces()) == 2)
                    self.assertEqual(workspaces()[0]["id"], second_id, "compaction preserves identity")
                    self.assertEqual([w["index"] for w in workspaces()], [1, 2])
                    command("workspace-back-and-forth")
                    self.assertEqual(active(), "1", "removed history cannot resurrect an empty workspace")
                    spare_id = workspaces()[1]["id"]
                    command("workspace", "99")
                    self.assertEqual(active(), "2")
                    self.assertEqual(next(w["id"] for w in workspaces() if w["focused"]), spare_id)
                    command("workspace-back-and-forth")
                    self.assertEqual(active(), "1")

                    replacement = source.with_suffix(".new")
                    replacement.write_text("animations { enabled #false; }\nworkspaces { auto-back-and-forth #true; }\n")
                    replacement.replace(source)
                    command("reload-config")
                    command("workspace", "1")
                    self.assertEqual(active(), "1", "explicit IPC selection must not auto-toggle")
                    command("workspace-back-and-forth")
                    self.assertEqual(active(), "2")
                    command("workspace-back-and-forth")
                    self.assertEqual(active(), "1")

                    os.killpg(second.pid, signal.SIGTERM)
                    second.wait(timeout=3)
                    wait_for(lambda: all(w["window_count"] == 0 for w in workspaces()))
                    command("workspace", "2")
                    wait_for(lambda: len(workspaces()) == 1)
                    self.assertEqual(workspaces()[0]["id"], spare_id)
                    self.assertEqual(workspaces()[0]["name"], "1")
                    self.assertTrue(workspaces()[0]["visible"])
                    self.assertTrue(workspaces()[0]["focused"])
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
