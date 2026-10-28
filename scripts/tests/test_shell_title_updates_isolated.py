"""Check focused and background title updates on a real GPU panel.

FERESE_TEST_SHELL_TITLES=1 python3 scripts/tests/test_shell_title_updates_isolated.py
Requires a Wayland host, cc, wayland-scanner, and dbus-daemon.
"""
import json
import os
from pathlib import Path
import re
import select
import shlex
import signal
import subprocess
import tempfile
import time
import unittest

from session_socket import ipc_environment, ipc_socket
from test_shell_motion_isolated import panel_config


@unittest.skipUnless(os.environ.get("FERESE_TEST_SHELL_TITLES") == "1", "opt-in nested shell test")
class ShellTitleUpdatesTest(unittest.TestCase):
    def test_titles_redraw_only_the_required_panel_frames(self):
        repo = Path(__file__).resolve().parents[2]
        binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/release/ferese")
        shell = repo / os.environ.get("FERESE_TEST_SHELL", "target/release/ferese-shell")
        ctl = repo / os.environ.get("FERESE_TEST_CTL", "target/release/feresectl")
        children = []
        with tempfile.TemporaryDirectory(prefix="ferese-shell-titles-") as temporary:
            root = Path(temporary)
            protocols = subprocess.check_output(
                ["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True).strip()
            xml = str(Path(protocols) / "stable/xdg-shell/xdg-shell.xml")
            for kind, output in [("client-header", "xdg-shell-client-protocol.h"),
                                 ("private-code", "xdg-shell-protocol.c")]:
                subprocess.run(["wayland-scanner", kind, xml, str(root / output)], check=True)
            flags = shlex.split(subprocess.check_output(
                ["pkg-config", "--cflags", "--libs", "wayland-client"], text=True))
            subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-I", str(root),
                            str(repo / "scripts/tests/fixtures/window-titles.c"),
                            str(root / "xdg-shell-protocol.c"), "-o", str(root / "client"), *flags], check=True)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            (config / "config.kdl").write_text(
                'status { keybinding-guide #false; }\n'
                'window-rule app-id="ferese.test.titles" floating=#true\n' + panel_config("flat"))
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
            env = {key: value for key, value in os.environ.items() if not key.startswith("FERESE_")}
            env.update(XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=str(display),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       XDG_CACHE_HOME=str(root / "cache"), XDG_DATA_HOME=str(root / "data"),
                       FERESE_COMPOSITOR_WALLPAPER="1", RUST_LOG="ferese=info")
            env.pop("WAYLAND_SOCKET", None)
            bus_config = root / "bus.conf"
            bus_config.write_text(
                '<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
                '<policy context="default"><allow send_destination="*"/>'
                '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
            log_path = root / "session.log"

            def launch(command, **kwargs):
                process = subprocess.Popen(command, env=ipc_environment(env), start_new_session=True, **kwargs)
                children.append(process)
                return process

            def wait_for(predicate):
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    value = predicate()
                    if value:
                        return value
                    time.sleep(.025)
                self.fail("Timed out:\n" + log_path.read_text()[-8000:])

            def control(*args):
                return subprocess.check_output([str(ctl), *args], env=ipc_environment(env), text=True, timeout=5)

            def snapshots():
                return log_path.read_text().count(".snapshot_end(")

            def frames():
                return len(re.findall(r"wl_surface[@#]\d+\.attach\(wl_buffer[@#]", log_path.read_text()))

            def title(index, value):
                client.stdin.write(f"{index} {value}\n")
                client.stdin.flush()
                self.assertTrue(select.select([client.stdout], [], [], 5)[0], "title client did not reply")
                self.assertEqual(client.stdout.readline().strip(), "changed")

            def burst(index, prefix, period=.05):
                for number in range(20):
                    title(index, f"{prefix} {number}")
                    time.sleep(period)
                time.sleep(.2)

            def wait_title(value):
                wait_for(lambda: any(window["title"] == value for window in json.loads(control("-j", "windows"))))

            def focused_title():
                return next((window["title"] for window in json.loads(control("-j", "windows"))
                             if window["focused"]), None)

            def watch(enabled):
                matches = re.findall(r"\.set_background_title_updates\(([01])\)", log_path.read_text())
                return matches and matches[-1] == str(int(enabled))

            try:
                with log_path.open("w") as log:
                    bus = launch(["dbus-daemon", "--config-file=" + str(bus_config), "--nofork", "--print-address=1"],
                                 stdout=subprocess.PIPE, stderr=log, text=True)
                    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                    compositor = launch([str(binary), "--backend", "nested", "--grant-effects", "--grant-shell-control",
                                         "--", "env", "WAYLAND_DEBUG=client", "ICED_BACKEND=wgpu", str(shell)],
                                        stdout=log, stderr=log)
                    wait_for(lambda: ipc_socket(runtime).is_socket() and watch(False))
                    child_display = wait_for(lambda: next((p for p in runtime.glob("wayland-*") if p.is_socket()), None))
                    env["WAYLAND_DISPLAY"] = str(child_display)
                    client = launch([str(root / "client")], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                    stderr=log, text=True)
                    self.assertTrue(select.select([client.stdout], [], [], 10)[0], "window client did not map")
                    self.assertEqual(client.stdout.readline().strip(), "ready")
                    wait_for(lambda: len(json.loads(control("-j", "windows"))) == 2)
                    wait_for(lambda: focused_title() == "Focused window")
                    time.sleep(.5)
                    before = snapshots()
                    burst(0, "Hidden spinner")
                    wait_title("Hidden spinner 19")
                    self.assertEqual(snapshots(), before, "background titles repainted the closed panel")
                    print("Background: 20 actual title changes produced no shell snapshots", flush=True)

                    before = snapshots()
                    title(1, "Current focused title")
                    wait_title("Current focused title")
                    wait_for(lambda: snapshots() > before)
                    self.assertIn('"Current focused title"', log_path.read_text())

                    time.sleep(.3)
                    before = frames()
                    burst(1, "Focused spinner", .1)
                    wait_title("Focused spinner 19")
                    self.assertIn('"Focused spinner 19"', log_path.read_text())
                    rendered = frames() - before
                    self.assertGreater(rendered, 0, "focused titles did not repaint the panel")
                    # Allow unrelated clock/status updates, but not a second
                    # GPU submission for every title received by the runtime.
                    self.assertLessEqual(rendered, 24, f"20 title changes submitted {rendered} frames")
                    print(f"Focused: 20 actual title changes submitted {rendered} panel frames", flush=True)
                    title(1, "Current focused title")
                    wait_title("Current focused title")

                    control("focus-last-window")
                    wait_for(lambda: focused_title() == "Hidden spinner 19")
                    wait_for(lambda: '"Hidden spinner 19"' in log_path.read_text())
                    control("focus-last-window")
                    wait_for(lambda: focused_title() == "Current focused title")
                    time.sleep(.2)

                    before = snapshots()
                    control("launcher", "toggle")
                    wait_for(lambda: watch(True) and snapshots() > before)
                    self.assertIn('"Hidden spinner 19"', log_path.read_text())
                    before = snapshots()
                    burst(0, "Visible list")
                    wait_title("Visible list 19")
                    wait_for(lambda: snapshots() > before)
                    self.assertIn('"Visible list 19"', log_path.read_text())
                    print("Launcher: opening fetched the latest title; live updates continued", flush=True)

                    control("launcher", "toggle")
                    wait_for(lambda: watch(False))
                    time.sleep(.5)
                    before = snapshots()
                    burst(0, "Closed list")
                    wait_title("Closed list 19")
                    self.assertEqual(snapshots(), before, "closing the launcher left a title subscription active")
                    self.assertIsNone(compositor.poll(), log_path.read_text()[-8000:])
                    self.assertNotIn("panicked at", log_path.read_text())
            finally:
                if destination := os.environ.get("FERESE_TEST_LOG"):
                    Path(destination).write_text(log_path.read_text() if log_path.exists() else "")
                for process in reversed(children):
                    if process.poll() is None:
                        os.killpg(process.pid, signal.SIGTERM)
                        try:
                            process.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            os.killpg(process.pid, signal.SIGKILL)
                            process.wait()
                    for stream in (process.stdin, process.stdout, process.stderr):
                        if stream is not None:
                            stream.close()


if __name__ == "__main__":
    unittest.main()
