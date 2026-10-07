"""Exercise real shell frame-driven motion on a private nested desktop and bus.

FERESE_TEST_SHELL_MOTION=1 python3 scripts/tests/test_shell_motion_isolated.py
Set FERESE_TEST_BAR_LAYOUT=islands to also test island masks and live switching.
Uses release binaries by default. Requires a Wayland host, dbus-daemon and gdbus.
Protocol traces contain only this test's shell traffic, never the host desktop.
"""
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import re
import signal
import subprocess
import tempfile
import time
import unittest


@unittest.skipUnless(os.environ.get("FERESE_TEST_SHELL_MOTION") == "1", "opt-in nested shell test")
class ShellMotionTest(unittest.TestCase):
    def test_notifications_and_modals_follow_frames_and_finish_closing(self):
        repo = Path(__file__).resolve().parents[2]
        binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/release/ferese")
        shell = repo / os.environ.get("FERESE_TEST_SHELL", "target/release/ferese-shell")
        ctl = repo / os.environ.get("FERESE_TEST_CTL", "target/release/feresectl")
        children = []
        with tempfile.TemporaryDirectory(prefix="ferese-shell-motion-") as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            bar_layout = os.environ.get("FERESE_TEST_BAR_LAYOUT", "continuous")
            self.assertIn(bar_layout, ("continuous", "islands"))
            config_file = config / "config.kdl"
            config_file.write_text(
                'animations { speed 0.5; }\n'
                f'status {{ keybinding-guide #false; bar-layout "{bar_layout}"; bar-island-padding 12; }}\n')
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=str(display),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       XDG_CACHE_HOME=str(root / "cache"), RUST_LOG="ferese=info,ferese::nested_input=debug")
            env.pop("WAYLAND_SOCKET", None)
            env.pop("FERESE_SHELL_CONTROL_SOCKET", None)
            log_path = root / "session.log"
            # No service directories: readiness checks must not auto-activate
            # the host's notification daemon on this private bus.
            bus_config = root / "bus.conf"
            bus_config.write_text(
                '<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
                '<policy context="default"><allow send_destination="*"/>'
                '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>')

            def launch(command, **kwargs):
                process = subprocess.Popen(command, env=ipc_environment(env), start_new_session=True, **kwargs)
                children.append(process)
                return process

            def wait_for(predicate, timeout=15):
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    value = predicate()
                    if value:
                        return value
                    time.sleep(.025)
                trace = log_path.read_text()
                relevant = '\n'.join(line for line in trace.splitlines() if any(
                    word in line for word in ('get_layer_surface', 'ERROR', 'WARN', 'notification')))
                self.fail("Timed out:\n" + relevant[-16000:] + '\n' + trace[-4000:])

            def dbus(method, *args, check=True):
                return subprocess.run(["gdbus", "call", "--session", "--dest", "org.freedesktop.Notifications",
                                       "--object-path", "/org/freedesktop/Notifications", "--method",
                                       "org.freedesktop.Notifications." + method, *args],
                                      env=ipc_environment(env), text=True, capture_output=True, timeout=5, check=check)

            def layer_surface(namespace):
                matches = list(re.finditer(r'get_layer_surface\([^\n]*?wl_surface[@#](\d+)[^\n]*"' + namespace + '"', log_path.read_text()))
                # Wayland object IDs can be reused after destruction. Restrict
                # assertions to this surface's lifetime in the trace.
                return (matches[-1].group(1), matches[-1].start()) if matches else None

            def frames(surface):
                identity, start = surface
                return len(re.findall(r'wl_surface[@#]' + identity + r'\.frame\(', log_path.read_text()[start:]))

            def commits(surface):
                identity, start = surface
                return len(re.findall(r'wl_surface[@#]' + identity + r'\.commit\(', log_path.read_text()[start:]))

            def destroyed(surface):
                identity, start = surface
                return re.search(r'wl_surface[@#]' + identity + r'\.destroy\(', log_path.read_text()[start:]) is not None

            def bar_region_count(surface):
                identity, start = surface
                trace = log_path.read_text()[start:]
                effects = re.findall(
                    r'get_surface_effects\(new id ferese_surface_effects_v1[@#](\d+), wl_surface[@#]'
                    + identity + r'\)', trace)
                if not effects:
                    return None

                updates = re.findall(
                    r'ferese_surface_effects_v1[@#]' + effects[-1] + r'\.set_regions\(array\[(\d+)\]\)', trace)
                return int(updates[-1]) // 20 if updates else None

            def bar_input_rectangles(surface):
                identity, start = surface
                trace = log_path.read_text()[start:]
                updates = list(re.finditer(
                    r'wl_surface[@#]' + identity + r'\.set_input_region\(wl_region[@#](\d+)\)', trace))
                if not updates:
                    return []

                update = updates[-1]
                region = update.group(1)
                creations = list(re.finditer(r'create_region\(new id wl_region[@#]' + region + r'\)', trace[:update.start()]))
                if not creations:
                    return []

                return [tuple(map(int, values)) for values in re.findall(
                    r'wl_region[@#]' + region + r'\.add\((-?\d+), (-?\d+), (\d+), (\d+)\)',
                    trace[creations[-1].start():update.start()])]

            with log_path.open("w") as log:
                try:
                    bus = launch(["dbus-daemon", "--config-file=" + str(bus_config), "--nofork", "--print-address=1"],
                                 stdout=subprocess.PIPE, stderr=log, text=True)
                    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                    compositor = launch([str(binary), "--backend", "nested", "--grant-effects", "--grant-shell-control",
                                         "--", "env", "WAYLAND_DEBUG=client", "ICED_BACKEND=tiny-skia", str(shell)],
                                        stdout=log, stderr=log)
                    wait_for(lambda: (ipc_socket(runtime)).is_socket())
                    wait_for(lambda: dbus("GetServerInformation", check=False).returncode == 0)
                    bar = wait_for(lambda: layer_surface("ferese-shell-top-bar"))
                    if bar_layout == "islands":
                        wait_for(lambda: bar_region_count(bar) == 2)
                        original_input = wait_for(lambda: bar_input_rectangles(bar))
                    time.sleep(.5)
                    reply = dbus("Notify", "Ferese motion test", "0", "", "Frame cadence", "Synthetic test notification", "[]", "{}", "0").stdout
                    notice = re.search(r'uint32 (\d+)', reply).group(1)
                    surface = wait_for(lambda: layer_surface("ferese-shell-notifications"))
                    time.sleep(1.5)
                    opening_frames = frames(surface)
                    self.assertGreater(opening_frames, 10, log_path.read_text()[-8000:])
                    time.sleep(1)
                    for attempt in range(3):
                        trace_start = len(log_path.read_text())
                        before = commits(surface)
                        time.sleep(2)
                        idle_frames = commits(surface) - before
                        if 'ferese::nested_input:' not in log_path.read_text()[trace_start:]:
                            break
                    else:
                        self.fail("Host input interrupted all three idle measurement intervals")
                    # Clock/status updates still redraw settled surfaces. Count
                    # commits: Iced can request two callbacks for one redraw.
                    self.assertLessEqual(idle_frames, 12, "settled toast kept requesting animation frames")
                    material_updates = log_path.read_text().count('.set_presentation(')
                    dbus("CloseNotification", notice)
                    wait_for(lambda: destroyed(surface), timeout=5)
                    self.assertGreater(log_path.read_text().count('.set_presentation(') - material_updates, 5,
                                       "notification materials did not follow their fade")
                    print(f"Toast: {opening_frames} opening callback requests, {idle_frames / 2:g} settled commits/s; destroyed", flush=True)

                    subprocess.run([str(ctl), "toggle-keybinding-guide"], env=ipc_environment(env), check=True, capture_output=True)
                    modal = wait_for(lambda: layer_surface("ferese-system-modal"))
                    wait_for(lambda: frames(modal) >= 4)
                    time.sleep(.3)
                    subprocess.run([str(ctl), "toggle-keybinding-guide"], env=ipc_environment(env), check=True, capture_output=True)
                    wait_for(lambda: destroyed(modal), timeout=5)
                    self.assertGreater(frames(modal), 10, "modal close did not follow frames")
                    self.assertIsNone(compositor.poll(), log_path.read_text()[-8000:])
                    self.assertNotIn("panicked at", log_path.read_text())
                    print(f"Modal: {frames(modal)} callbacks; interrupted opening closed and destroyed", flush=True)

                    if bar_layout == "islands":
                        for layout_name, expected_count in [("continuous", 1), ("islands", None)]:
                            source = config_file.read_text()
                            config_file.write_text(re.sub(
                                r'bar-layout "[^"]+"', f'bar-layout "{layout_name}"', source))
                            subprocess.run([str(ctl), "reload-config"], env=ipc_environment(env), check=True, capture_output=True)
                            wait_for(lambda: bar_region_count(bar) == (expected_count or 2))

                        self.assertIsNone(compositor.poll(), log_path.read_text()[-8000:])
                        self.assertNotIn("panicked at", log_path.read_text())
                        print("Bar: separate material regions; continuous/islands live switches passed", flush=True)

                        for padding in [4, 0]:
                            source = config_file.read_text()
                            config_file.write_text(re.sub(
                                r'bar-island-padding \d+', f'bar-island-padding {padding}', source))
                            subprocess.run([str(ctl), "reload-config"], env=ipc_environment(env), check=True, capture_output=True)
                            expected_width = original_input[0][2] - 2 * (12 - padding)
                            wait_for(lambda: len(bar_input_rectangles(bar)) == 2
                                     and bar_input_rectangles(bar)[0][2] == expected_width)

                        print("Bar: live padding reductions updated the input regions", flush=True)
                finally:
                    for child in reversed(children):
                        if child.poll() is None:
                            os.killpg(child.pid, signal.SIGTERM)
                            try:
                                child.wait(timeout=5)
                            except subprocess.TimeoutExpired:
                                os.killpg(child.pid, signal.SIGKILL)
                                child.wait(timeout=5)
                        if child.stdout is not None:
                            child.stdout.close()


if __name__ == "__main__":
    unittest.main()
