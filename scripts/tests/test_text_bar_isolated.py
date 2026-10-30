"""Text bar protocol, effects and reload checks in a private nested session.

FERESE_TEST_TEXT_BAR=1 python3 scripts/tests/test_text_bar_isolated.py
Uses the same binary override variables as the other isolated shell tests.
Requires a Wayland host, dbus-daemon and Python Pillow.
"""
import io
import json
import re
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest

from session_socket import ipc_environment, ipc_socket
from PIL import Image, ImageColor


@unittest.skipUnless(os.environ.get("FERESE_TEST_TEXT_BAR") == "1", "opt-in nested text bar test")
class TextBarTest(unittest.TestCase):
    def test_layout_updates_and_preset_reload(self):
        repo = Path(__file__).resolve().parents[2]
        binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/release/ferese")
        shell = repo / os.environ.get("FERESE_TEST_SHELL", "target/release/ferese-shell")
        ctl = repo / os.environ.get("FERESE_TEST_CTL", "target/release/feresectl")
        children = []
        with tempfile.TemporaryDirectory(prefix="ferese-text-bar-") as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese/config.kdl"
            config.parent.mkdir(parents=True)

            def configure(presentation, family="ferese-blue"):
                config.write_text('layout-mode "scrolling"\nstatus { keybinding-guide #false; }\n'
                                  f'theme {{ mode "dark"; family "{family}"; }}\n'
                                  f'panel main {{ style "flat"; presentation "{presentation}"; }}\n')

            configure("text")
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
            env = {key: value for key, value in os.environ.items() if not key.startswith("FERESE_")}
            env.update(XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=str(display),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       XDG_CACHE_HOME=str(root / "cache"), XDG_DATA_HOME=str(root / "data"),
                       DBUS_SYSTEM_BUS_ADDRESS="unix:path=/tmp/ferese-no-system-bus",
                       FERESE_COMPOSITOR_WALLPAPER="1", FERESE_ENABLE_SCREENCOPY="1", RUST_LOG="ferese=info")
            env.pop("WAYLAND_SOCKET", None)
            bus_config = root / "bus.conf"
            bus_config.write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
                                 '<policy context="default"><allow send_destination="*"/>'
                                 '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
            log_path = root / "session.log"

            def launch(command, **kwargs):
                process = subprocess.Popen(command, env=ipc_environment(env), start_new_session=True, **kwargs)
                children.append(process)
                return process

            def wait_for(predicate):
                deadline = time.monotonic() + 25
                while time.monotonic() < deadline:
                    if predicate():
                        return
                    time.sleep(.05)
                self.fail("Timed out:\n" + log_path.read_text()[-10000:])

            def control(*args):
                return subprocess.check_output([str(ctl), *args], env=ipc_environment(env), timeout=10)

            try:
                with log_path.open("w") as log:
                    bus = launch(["dbus-daemon", "--config-file=" + str(bus_config), "--nofork", "--print-address=1"],
                                 stdout=subprocess.PIPE, stderr=log, text=True)
                    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                    compositor = launch([str(binary), "--backend", "nested", "--grant-effects", "--grant-shell-control",
                                         "--", "env", "WAYLAND_DEBUG=client", "ICED_BACKEND=wgpu", str(shell)], stdout=log, stderr=log)
                    wait_for(lambda: ipc_socket(runtime).is_socket() and '.workspace_layout(' in log_path.read_text()
                             and '.set_exclusive_zone(24)' in log_path.read_text())
                    surface = re.search(r'get_layer_surface\([^,]+, wl_surface[#@](\d+),[^\n]+"ferese-shell-top-bar"', log_path.read_text()).group(1)
                    wait_for(lambda: re.search(r'wl_surface[#@]' + surface + r'\.attach\(wl_buffer', log_path.read_text()))
                    before = json.loads(control("-j", "workspaces"))
                    self.assertLess(len(before), 9, "text presentation must not create persistent workspaces")
                    self.assertIn('"scrolling"', log_path.read_text())
                    self.assertNotIn('.set_panel_presentation(', log_path.read_text(), "text bar enabled compositor material")
                    control("toggle-layout")
                    wait_for(lambda: '"tree"' in log_path.read_text())
                    control("toggle-layout")
                    def target_colors():
                        tokens = json.loads(control("-j", "theme", "get"))["theme"]["tokens"]
                        background = ImageColor.getcolor(tokens["surface"]["bar"]["background"], "RGBA")[:3]
                        accent = ImageColor.getcolor(tokens["colors"]["accent"], "RGBA")
                        alpha = accent[3] / 255
                        accent = tuple(round(accent[i] * alpha + background[i] * (1 - alpha)) for i in range(3))
                        return accent, background

                    def wait_for_colors(expected):
                        png = None
                        actual = None

                        def rendered():
                            nonlocal png, actual
                            png = control("screenshot")
                            image = Image.open(io.BytesIO(png)).convert("RGB")
                            actual = image.getpixel((2, 2)), image.getpixel((image.width - 2, 2))
                            return actual == expected

                        try:
                            wait_for(rendered)
                        except AssertionError as error:
                            raise AssertionError(f"Bar colors: expected {expected}, rendered {actual}") from error
                        return png

                    for mode in ("light", "dark"):
                        control("theme", "mode", mode)
                        wait_for(lambda: json.loads(control("-j", "theme", "get"))["theme"]["appearance"] == mode)
                        png = wait_for_colors(target_colors())
                        if destination := os.environ.get("FERESE_TEST_TEXT_BAR_PREVIEWS"):
                            Path(destination).mkdir(parents=True, exist_ok=True)
                            (Path(destination) / f"nested-{mode}.png").write_bytes(png)
                    # A family change must repaint even when appearance stays dark.
                    previous = target_colors()
                    for family in ("catppuccin", "gruvbox"):
                        configure("text", family)
                        control("reload-config")
                        snapshot = json.loads(control("-j", "theme", "get"))
                        self.assertEqual(snapshot["theme"]["appearance"], "dark")
                        expected = target_colors()
                        self.assertNotEqual(expected, previous, "fixture families must have distinct colors")
                        wait_for_colors(expected)
                        previous = expected
                    configure("standard")
                    control("reload-config")
                    wait_for(lambda: '.set_panel_presentation(' in log_path.read_text())
                    offset = len(log_path.read_text())
                    configure("text")
                    control("reload-config")
                    wait_for(lambda: '.clear_role(' in log_path.read_text()[offset:]
                             and '.set_exclusive_zone(24)' in log_path.read_text()[offset:])
                    self.assertEqual(json.loads(control("-j", "workspaces")), before)
                    self.assertIsNone(compositor.poll())
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
