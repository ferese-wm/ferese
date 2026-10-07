"""Check settled overview damage against real clients on a private nested desktop.

FERESE_TEST_OVERVIEW_DAMAGE=1 FERESE_TEST_BINARY=target/release/ferese \
    FERESE_TEST_CTL=target/release/feresectl python3 scripts/tests/test_overview_damage_isolated.py
Set FERESE_TEST_TERMINAL=alacritty to use Alacritty instead of foot.
Requires a Wayland host, the selected terminal and dbus-daemon.
Never connects clients to the host.
"""
import errno
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time
import unittest


@unittest.skipUnless(os.environ.get("FERESE_TEST_OVERVIEW_DAMAGE") == "1", "nested damage test is opt-in")
class OverviewDamageTest(unittest.TestCase):
    def test_stationary_overview_stops_damage_but_live_content_still_repaints(self):
        repo = Path(__file__).resolve().parents[2]
        binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/debug/ferese")
        control = repo / os.environ.get("FERESE_TEST_CTL", "target/debug/feresectl")
        terminal = os.environ.get("FERESE_TEST_TERMINAL", "foot")
        self.assertIn(terminal, ("foot", "alacritty"))
        children = []
        with tempfile.TemporaryDirectory(prefix="ferese-overview-damage-") as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            (config / "config.kdl").write_text("animations { enabled #true; }\n")
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(os.environ["XDG_RUNTIME_DIR"]) / display

            env = dict(os.environ, WAYLAND_DISPLAY=str(display), XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       FERESE_TRACE_PERFORMANCE="1", RUST_LOG="ferese=info,ferese::nested_input=debug")
            env.pop("WAYLAND_SOCKET", None)
            env.pop("FERESE_SHELL_CONTROL_SOCKET", None)
            log_path = root / "compositor.log"

            def launch(command, **kwargs):
                child = subprocess.Popen(command, env=env, start_new_session=True, **kwargs)
                children.append(child)
                return child

            def command(*args):
                return json.loads(subprocess.check_output([str(control), "-j", *args], env=env, timeout=5))

            def wait_for(predicate, seconds=12):
                deadline = time.monotonic() + seconds
                while time.monotonic() < deadline:
                    result = predicate()
                    if result is not None and result is not False:
                        return result

                    time.sleep(.025)

                self.fail("Timed out waiting for compositor state:\n" + log_path.read_text())

            def reports():
                return [dict((key, int(value)) for key, value in re.findall(r"(\w+)=(\d+)", line))
                        for line in log_path.read_text().splitlines() if "render performance" in line]

            def stationary_interval():
                # The first report includes time before this call. The second
                # covers a full five seconds of the requested stationary scene.
                count = len(reports())
                for attempt in range(3):
                    wait_for(lambda: len(reports()) >= count + 2)
                    lines = log_path.read_text().splitlines()
                    boundaries = [index for index, line in enumerate(lines)
                                  if "render performance" in line]
                    interval = lines[boundaries[-2] + 1:boundaries[-1]]
                    if not any("ferese::nested_input:" in line for line in interval):
                        return reports()[-1]
                    print("Discarding interval with host input; waiting for a quiet interval", flush=True)
                    count = len(boundaries) - 1
                self.fail("No input-free interval for the stationary overview check")

            def assert_quiet(label, report):
                print(label + ":", report, flush=True)
                with self.subTest(stage=label):
                    self.assertEqual(report["frames"], 0, str(report) + "\n" + log_path.read_text()[-5000:])
                    self.assertEqual(report["damaged_pixels"], 0, str(report))
                    self.assertGreater(report["no_damage_frames"], 0, str(report))

            writer = None
            with log_path.open("w") as log:
                try:
                    bus = launch(["dbus-daemon", "--session", "--nofork", "--print-address=1"],
                                 stdout=subprocess.PIPE, text=True)
                    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                    launch([str(binary), "--backend", "nested"], stdout=log, stderr=log)
                    wait_for(lambda: (runtime / "ferese/control.sock").is_socket())
                    env["WAYLAND_DISPLAY"] = str(next(
                        path for path in runtime.glob("wayland-*") if not path.name.endswith(".lock")
                    ))

                    fifo = root / "content"
                    os.mkfifo(fifo)
                    for index in range(4):
                        app = f"ferese.test.overview.{index}"
                        client = (["sh", "-c", 'cat "$1"; sleep 120', "sh", str(fifo)]
                                  if index == 0 else ["sleep", "120"])
                        terminal_command = (["alacritty", "--class", app, "--command", *client]
                                            if terminal == "alacritty" else ["foot", "--app-id", app, *client])
                        launch(terminal_command, stdout=log, stderr=log)
                        wait_for(lambda: any(window["app_id"] == app and window["width"] > 0
                                             for window in command("get-windows")))

                    def open_writer():
                        try:
                            return os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
                        except OSError as error:
                            if error.errno != errno.ENXIO:  # Reader has not opened yet.
                                raise

                            return None

                    writer = wait_for(open_writer)
                    command("toggle-overview")
                    time.sleep(2)
                    quiet = stationary_interval()
                    assert_quiet("Settled overview", quiet)

                    count = len(reports())
                    os.write(writer, b"Live overview content must still update.\n")
                    wait_for(lambda: len(reports()) > count)
                    changed = reports()[-1]
                    print("Live content update:", changed, flush=True)
                    with self.subTest(stage="Live content update"):
                        self.assertGreater(changed["frames"], 0, str(changed))
                        self.assertGreater(changed["damaged_pixels"], 0, str(changed))

                    # The content commit must not start another perpetual repaint.
                    quiet = stationary_interval()
                    assert_quiet("Settled overview after content update", quiet)

                    # Exercise spring dismissal and reverse it while still in
                    # flight, then dismiss again and check the settled desktop.
                    command("toggle-overview")
                    time.sleep(.08)
                    command("toggle-overview")
                    time.sleep(.08)
                    command("toggle-overview")
                    time.sleep(2)
                    quiet = stationary_interval()
                    assert_quiet("Settled desktop after overview reversals", quiet)
                finally:
                    if writer is not None:
                        os.close(writer)

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
