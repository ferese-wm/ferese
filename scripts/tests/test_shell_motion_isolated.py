"""Exercise real shell frame-driven motion on a private nested desktop and bus.

FERESE_TEST_SHELL_MOTION=1 python3 scripts/tests/test_shell_motion_isolated.py
Set FERESE_TEST_BAR_LAYOUT=floating to also test island masks and live switching.
Set FERESE_TEST_SHELL_BACKEND=wgpu to exercise GPU rendering.
Uses release binaries by default. Requires a Wayland host, dbus-daemon and gdbus.
Protocol traces contain only this test's shell traffic, never the host desktop.
"""
import json
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import re
import signal
import shutil
import subprocess
import tempfile
import time
import unittest


def panel_config(preset):
    style = preset
    return f'''panel main {{
    style "{style}"
    start {{ group navigation {{ island-padding 12; item overview kind=overview; item spaces kind=workspaces; }}; }}
    center {{ group title {{ island-padding 12; item title kind=focused-window; }}; }}
    end {{ group status {{ island-padding 12; item media kind=media; item quick kind=quick-settings; item network kind=network; item audio kind=audio; item recording kind=recording; item notifications kind=notifications; item battery kind=battery; item clock kind=clock; item display kind=display-mode; }}; }}
}}
'''


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
            bar_layout = os.environ.get("FERESE_TEST_BAR_LAYOUT", "flat")
            shell_backend = os.environ.get("FERESE_TEST_SHELL_BACKEND", "tiny-skia")
            self.assertIn(bar_layout, ("flat", "floating"))
            config_file = config / "config.kdl"
            config_file.write_text(
                'animations { speed 0.5; }\n'
                'status { keybinding-guide #false; }\n' + panel_config(bar_layout))
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
                matches = layer_surfaces(namespace)
                # Wayland object IDs can be reused after destruction. Restrict
                # assertions to this surface's lifetime in the trace.
                return matches[-1] if matches else None

            def layer_surfaces(namespace):
                return [(match.group(1), match.start()) for match in re.finditer(
                    r'get_layer_surface\([^\n]*?wl_surface[@#](\d+)[^\n]*"' + namespace + '"',
                    log_path.read_text())]

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
                    r'ferese_surface_effects_v1[@#]' + effects[-1] + r'\.set_panel_presentation\(array\[(\d+)\],', trace)
                return int(updates[-1]) // 20 if updates else None

            def bar_size(surface):
                identity, start = surface
                trace = log_path.read_text()[start:]
                match = re.search(r'get_layer_surface\(new id zwlr_layer_surface_v1[@#](\d+), wl_surface[@#]' + identity + r',', trace)
                if not match:
                    return None
                updates = re.findall(r'zwlr_layer_surface_v1[@#]' + match.group(1) + r'\.set_size\((\d+), (\d+)\)', trace)
                return tuple(map(int, updates[-1])) if updates else None

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

                rectangles = [tuple(map(int, values)) for values in re.findall(
                    r'wl_region[@#]' + region + r'\.add\((-?\d+), (-?\d+), (\d+), (\d+)\)',
                    trace[creations[-1].start():update.start()])]
                # Wayland regions are unions. Decorated groups can include
                # redundant child control rectangles without changing coverage.
                def contains(outer, inner):
                    x, y, width, height = outer
                    ix, iy, iw, ih = inner
                    return x <= ix and y <= iy and x + width >= ix + iw and y + height >= iy + ih
                return [rectangle for index, rectangle in enumerate(rectangles)
                        if not any(other != rectangle and contains(other, rectangle)
                                   or other == rectangle and other_index < index
                                   for other_index, other in enumerate(rectangles))]

            def bar_input_size(surface):
                rectangles = bar_input_rectangles(surface)
                if not rectangles:
                    return None
                return (max(x + width for x, y, width, height in rectangles)
                        - min(x for x, y, width, height in rectangles),
                        max(y + height for x, y, width, height in rectangles)
                        - min(y for x, y, width, height in rectangles))

            def bar_motion_settled(surface):
                updates = log_path.read_text().count('.set_panel_presentation(')
                before = commits(surface)
                time.sleep(.25)
                return (log_path.read_text().count('.set_panel_presentation(') == updates
                        and commits(surface) - before <= 2)


            with log_path.open("w") as log:
                try:
                    bus = launch(["dbus-daemon", "--config-file=" + str(bus_config), "--nofork", "--print-address=1"],
                                 stdout=subprocess.PIPE, stderr=log, text=True)
                    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                    compositor = launch([str(binary), "--backend", "nested", "--grant-effects", "--grant-shell-control",
                                         "--", "env", "WAYLAND_DEBUG=client", f"ICED_BACKEND={shell_backend}", str(shell)],
                                        stdout=log, stderr=log)
                    wait_for(lambda: (ipc_socket(runtime)).is_socket())
                    wait_for(lambda: dbus("GetServerInformation", check=False).returncode == 0)
                    bar = wait_for(lambda: layer_surface("ferese-shell-top-bar"))
                    if bar_layout == "floating":
                        wait_for(lambda: bar_region_count(bar) == 2)
                        original_input = wait_for(lambda: bar_input_rectangles(bar))
                        original_start_width = max(x + width for x, y, width, height in original_input if x < 300) - min(x for x, y, width, height in original_input if x < 300)
                    time.sleep(.5)
                    reply = dbus("Notify", "Ferese motion test", "0", "", "Frame cadence", "Synthetic test notification", "[]", "{}", "0").stdout
                    notice = re.search(r'uint32 (\d+)', reply).group(1)
                    surface = wait_for(lambda: layer_surface("ferese-shell-notifications"))
                    time.sleep(1.5)
                    opening_frames = frames(surface)
                    # Require multiple animation frames, without imposing a
                    # hardware-dependent frame rate on software-rendered CI.
                    self.assertGreaterEqual(opening_frames, 4, log_path.read_text()[-8000:])
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
                    self.assertGreater(log_path.read_text().count('.set_presentation(') - material_updates, 1,
                                       "notification materials did not follow their fade")
                    print(f"Toast: {opening_frames} opening callback requests, {idle_frames / 2:g} settled commits/s; destroyed", flush=True)

                    subprocess.run([str(ctl), "launcher", "toggle"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                    launcher = wait_for(lambda: layer_surface("ferese-launcher"))
                    wait_for(lambda: frames(launcher) >= 4)
                    time.sleep(1.5)
                    if capture_path := os.environ.get("FERESE_TEST_LAUNCHER_IMAGE"):
                        with open(capture_path, "wb") as capture:
                            subprocess.run([str(ctl), "screenshot"], env=ipc_environment(env), stdout=capture,
                                           stderr=subprocess.PIPE, check=True, timeout=10)
                    before = commits(launcher)
                    time.sleep(1)
                    self.assertLessEqual(commits(launcher) - before, 10, "settled launcher kept animating")
                    before_close = frames(launcher)
                    subprocess.run([str(ctl), "launcher", "toggle"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                    wait_for(lambda: destroyed(launcher), timeout=5)
                    self.assertGreater(frames(launcher), before_close, "launcher did not animate its close")
                    print("Launcher: animated open and close, idle timer stopped, surface destroyed", flush=True)

                    subprocess.run([str(ctl), "toggle-keybinding-guide"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                    modal = wait_for(lambda: layer_surface("ferese-system-modal"))
                    wait_for(lambda: frames(modal) >= 4)
                    time.sleep(.3)
                    before_close = frames(modal)
                    subprocess.run([str(ctl), "toggle-keybinding-guide"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                    wait_for(lambda: destroyed(modal), timeout=5)
                    self.assertGreater(frames(modal), before_close, "modal close did not follow frames")
                    self.assertIsNone(compositor.poll(), log_path.read_text()[-8000:])
                    self.assertNotIn("panicked at", log_path.read_text())
                    print(f"Modal: {frames(modal)} callbacks; interrupted opening closed and destroyed", flush=True)

                    # A toast can resize or start closing before its previous
                    # software buffer has caught up with the new viewport.
                    for index in range(40):
                        reply = dbus("Notify", "Ferese motion test", "0", "", "Rapid toast",
                                     "Short body", "[]", "{}", "0").stdout
                        notice = re.search(r'uint32 (\d+)', reply).group(1)
                        if index % 2 == 0:
                            dbus("Notify", "Ferese motion test", notice, "", "Updated toast",
                                 "A longer notification body that wraps over several lines. " * 6,
                                 "[]", "{}", "0")
                        dbus("CloseNotification", notice)
                        time.sleep(.015)
                    time.sleep(.5)
                    self.assertIsNone(compositor.poll(), log_path.read_text()[-8000:])
                    self.assertEqual(dbus("GetServerInformation", check=False).returncode, 0)
                    self.assertNotIn("panicked at", log_path.read_text())
                    print("Toast: 40 rapid open/update/close cycles completed", flush=True)

                    for _ in range(40):
                        subprocess.run([str(ctl), "toggle-keybinding-guide"],
                                       env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                        time.sleep(.03)
                        subprocess.run([str(ctl), "toggle-keybinding-guide"],
                                       env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                        time.sleep(.06)
                    # A final toggle may leave the owned modal closing. Let
                    # that motion finish before deciding whether a live modal
                    # needs another toggle; toggling during closing reopens it.
                    deadline = time.monotonic() + 5
                    while True:
                        remaining = [surface for surface in layer_surfaces("ferese-system-modal")
                                     if not destroyed(surface)]
                        if not remaining or time.monotonic() >= deadline:
                            break
                        time.sleep(.05)
                    self.assertLessEqual(len(remaining), 1, "cancelled modals retained native surfaces")
                    if remaining:
                        subprocess.run([str(ctl), "toggle-keybinding-guide"],
                                       env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                        wait_for(lambda: destroyed(remaining[0]))
                    print("Modal: rapid toggles released all native surfaces", flush=True)

                    if bar_layout == "floating":
                        for layout_name, expected_count in [("flat", 1), ("floating", None)]:
                            config_file.write_text('animations { speed 0.5; }\nstatus { keybinding-guide #false; }\n' + panel_config(layout_name))
                            subprocess.run([str(ctl), "reload-config"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                            wait_for(lambda: bar_region_count(bar) == (expected_count or 2))

                        self.assertIsNone(compositor.poll(), log_path.read_text()[-8000:])
                        self.assertNotIn("panicked at", log_path.read_text())
                        print("Bar: separate material regions; flat/floating live switches passed", flush=True)

                        for padding in [4, 0]:
                            source = config_file.read_text()
                            config_file.write_text(re.sub(
                                r'island-padding \d+', f'island-padding {padding}', source))
                            subprocess.run([str(ctl), "reload-config"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                            expected_width = original_start_width - 2 * (12 - padding)
                            def start_width():
                                rectangles = [(x, width) for x, y, width, height in bar_input_rectangles(bar) if x < 300]
                                return max((x + width for x, width in rectangles), default=0) - min((x for x, width in rectangles), default=0)
                            wait_for(lambda: start_width() == expected_width)

                        print("Bar: live padding reductions updated the input regions", flush=True)
                    if settings_binary := os.environ.get("FERESE_TEST_SETTINGS"):
                        child_display = next(path for path in runtime.glob("wayland-*") if path.is_socket())
                        settings_env = dict(ipc_environment(env), WAYLAND_DISPLAY=str(child_display), WINIT_UNIX_BACKEND="wayland", ICED_BACKEND="wgpu")
                        settings_env.pop("WAYLAND_SOCKET", None)
                        settings_env.pop("FERESE_SHELL_CONTROL_SOCKET", None)
                        settings = subprocess.Popen([str(repo / settings_binary), "--page", "bar"], env=settings_env,
                                                    start_new_session=True, stdout=log, stderr=log)
                        children.append(settings)
                        def windows():
                            response = subprocess.run([str(ctl), "--json", "windows"], env=ipc_environment(env),
                                                      text=True, capture_output=True, check=True, timeout=5)
                            return json.loads(response.stdout)
                        wait_for(lambda: any(window["mapped"] and window["app_id"] == "dev.ferese.Settings" for window in windows()))
                        config_file.write_text('status { keybinding-guide #false; }\n' + panel_config("floating").replace('style "floating"', 'style "adaptive"'))
                        subprocess.run([str(ctl), "reload-config"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                        wait_for(lambda: bar_region_count(bar) == 3)
                        for command, count in [("toggle-maximized", 1), ("toggle-maximized", 3)]:
                            subprocess.run([str(ctl), command], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                            wait_for(lambda: bar_region_count(bar) == count)
                        subprocess.run([str(ctl), "toggle-fullscreen"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                        wait_for(lambda: not bar_input_rectangles(bar))
                        subprocess.run([str(ctl), "toggle-fullscreen"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                        wait_for(lambda: bar_input_rectangles(bar))
                        settings.terminate()
                        settings.wait(timeout=5)
                        wait_for(lambda: not windows())
                        print("Adaptive: maximization changed the real panel; fullscreen released input and restored it", flush=True)

                    if bar_layout == "floating":
                        for style, regions in [("floating", 2), ("capsules", 2), ("notch", 2), ("flat", 1), ("fused", 2), ("adaptive", 2)]:
                            config_file.write_text('animations { speed 0.5; }\nstatus { keybinding-guide #false; }\n' + panel_config(style))
                            subprocess.run([str(ctl), "reload-config"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                            wait_for(lambda: bar_region_count(bar) == regions)
                            self.assertTrue(bar_input_rectangles(bar), style + " lost its native input")
                        config_file.write_text('animations { speed 0.5; }\nstatus { keybinding-guide #false; }\nnotifications { timeout-ms 1200; }\n'
                                               + panel_config("morph").replace('style "morph"', 'style "morph"\n    morph { peek-on-hover #false; }'))
                        subprocess.run([str(ctl), "reload-config"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                        # The visible hub follows its measured content; its larger
                        # backing surface remains allocated to avoid resize flashes.
                        wait_for(lambda: (size := bar_input_size(bar)) and 0 < size[0] < 320
                                 and size[1] == 34 and bar_region_count(bar) == 1)
                        reply = dbus("Notify", "Morph test", "0", "", "Hub notification", "Bounded native surface", "[]", "{}", "0").stdout
                        notice = re.search(r'uint32 (\d+)', reply).group(1)
                        wait_for(lambda: bar_input_size(bar) == (320, 48))
                        allocation = bar_size(bar)
                        self.assertGreaterEqual(allocation[0], 320)
                        self.assertGreaterEqual(allocation[1], 48)
                        # Removing a notice must collapse its hub immediately; an old timeout cannot reopen it.
                        dbus("CloseNotification", notice)
                        wait_for(lambda: (size := bar_input_size(bar)) and 0 < size[0] < 320 and size[1] == 34)
                        self.assertEqual(bar_size(bar), allocation, "closing Morph resized its retained backing surface")
                        # Rounded protocol geometry can stop changing while subpixel
                        # spring motion or the content fade still needs frames.
                        wait_for(lambda: bar_motion_settled(bar))
                        before = commits(bar)
                        trace_start = len(log_path.read_text())
                        time.sleep(.5)
                        # Clock/service updates can cause a short render burst; settled geometry must not animate.
                        self.assertNotIn(".set_panel_presentation(", log_path.read_text()[trace_start:])
                        self.assertLessEqual(commits(bar) - before, 4, "settled Morph keeps scheduling frames")
                        self.assertEqual(bar_region_count(bar), 1)
                        config_file.write_text('status { keybinding-guide #false; }\n' + panel_config("floating"))
                        subprocess.run([str(ctl), "reload-config"], env=ipc_environment(env), check=True, capture_output=True, timeout=5)
                        wait_for(lambda: bar_region_count(bar) == 2)
                        print("Styles: native masks survived every live switch; Morph grew, closed and stopped frames", flush=True)

                    saved = config_file.read_text()
                    config_file.write_text('panel main { surface "invalid-surface"; }\n')
                    rejected = subprocess.run([str(ctl), "reload-config"], env=ipc_environment(env),
                                              capture_output=True, text=True, timeout=5)
                    self.assertNotEqual(rejected.returncode, 0, "invalid reload was accepted")
                    self.assertIn("invalid_config", rejected.stderr)
                    self.assertFalse(destroyed(bar), "invalid reload destroyed the current panel")
                    self.assertEqual(dbus("GetServerInformation", check=False).returncode, 0)
                    config_file.write_text(saved)
                    subprocess.run([str(ctl), "reload-config"], env=ipc_environment(env),
                                   check=True, capture_output=True, timeout=5)
                    subprocess.run([str(ctl), "toggle-keybinding-guide"], env=ipc_environment(env),
                                   check=True, capture_output=True, timeout=5)
                    recovered = wait_for(lambda: (surface := layer_surface("ferese-system-modal"))
                                         and not destroyed(surface) and surface)
                    subprocess.run([str(ctl), "toggle-keybinding-guide"], env=ipc_environment(env),
                                   check=True, capture_output=True, timeout=5)
                    wait_for(lambda: destroyed(recovered))
                    self.assertIsNone(compositor.poll(), log_path.read_text()[-8000:])
                    self.assertNotIn("panicked at", log_path.read_text())
                    print("Reload: invalid config preserved the panel; repaired config reopened and released a modal", flush=True)
                finally:
                    if destination := os.environ.get("FERESE_TEST_LOG"):
                        shutil.copyfile(log_path, destination)
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
                    if directory := os.environ.get("FERESE_TEST_LOG_DIR"):
                        logs = Path(directory)
                        logs.mkdir(parents=True, exist_ok=True)
                        shutil.copyfile(log_path, logs / f"shell-{bar_layout}.log")


if __name__ == "__main__":
    unittest.main()
