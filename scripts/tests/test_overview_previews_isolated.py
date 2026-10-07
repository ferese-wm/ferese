"""Check active and inactive workspace thumbnails on a private nested desktop.

FERESE_TEST_OVERVIEW_PREVIEWS=1 FERESE_TEST_BINARY=target/release/ferese \
    FERESE_TEST_CTL=target/release/feresectl python3 scripts/tests/test_overview_previews_isolated.py
Requires a Wayland host, cc, wayland-scanner and Pillow.
"""
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest


@unittest.skipUnless(os.environ.get("FERESE_TEST_OVERVIEW_PREVIEWS") == "1", "nested preview test is opt-in")
class OverviewPreviews(unittest.TestCase):
    def test_strip_keeps_content_from_inactive_workspaces(self):
        from PIL import Image

        repo = Path(__file__).resolve().parents[2]
        binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/debug/ferese")
        ctl = repo / os.environ.get("FERESE_TEST_CTL", "target/debug/feresectl")
        children = []
        with tempfile.TemporaryDirectory(prefix="ferese-overview-previews-") as directory:
            root = Path(directory)
            protocols = subprocess.check_output(
                ["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True
            ).strip()
            xml = str(Path(protocols) / "stable/xdg-shell/xdg-shell.xml")
            for kind, name in [("client-header", "xdg-shell-client-protocol.h"),
                               ("private-code", "xdg-shell-protocol.c")]:
                subprocess.run(["wayland-scanner", kind, xml, str(root / name)], check=True)

            flags = subprocess.check_output(["pkg-config", "--cflags", "--libs", "wayland-client"], text=True).split()
            subprocess.run(["cc", "-Wall", "-Wextra", "-I", str(root),
                            str(repo / "scripts/tests/fixtures/window-capture.c"),
                            str(root / "xdg-shell-protocol.c"), "-o", str(root / "client"), *flags], check=True)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese/config.kdl"
            config.parent.mkdir(parents=True)
            config.write_text('window-rule app-id="ferese.test.window-capture" floating=#true\n')
            host = Path(os.environ["WAYLAND_DISPLAY"])
            if not host.is_absolute():
                host = Path(os.environ["XDG_RUNTIME_DIR"]) / host

            env = dict(os.environ, WAYLAND_DISPLAY=str(host), XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       XDG_DATA_HOME=str(root / "data"), FERESE_ENABLE_SCREENCOPY="1")
            env.pop("WAYLAND_SOCKET", None)
            env.pop("FERESE_SHELL_CONTROL_SOCKET", None)
            log_path = root / "compositor.log"

            def call(*args):
                return subprocess.check_output([str(ctl), "-j", *args], env=env, timeout=10)

            def wait_for(predicate):
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    if predicate():
                        return

                    time.sleep(.025)

                self.fail("Timed out waiting for private compositor:\n" + log_path.read_text())

            with log_path.open("w") as log:
                def launch(command):
                    child = subprocess.Popen(command, env=env, stdout=log, stderr=log, start_new_session=True)
                    children.append(child)

                try:
                    launch([str(binary), "--backend", "nested"])
                    wait_for(lambda: (runtime / "ferese/control.sock").is_socket())
                    env["WAYLAND_DISPLAY"] = str(next(path for path in runtime.glob("wayland-*") if path.is_socket()))
                    launch([str(root / "client")])
                    wait_for(lambda: len(json.loads(call("get-windows"))) == 1)
                    call("workspace", "2")
                    launch([str(root / "client"), "cover"])
                    wait_for(lambda: len(json.loads(call("get-windows"))) == 2)
                    call("toggle-overview")
                    output = json.loads(call("get-outputs"))[0]
                    scale = output["scale"]
                    strip_bottom = (16 + min(132, max(40, output["height"] * .24))) * scale
                    for workspace in (2, 1):
                        if workspace == 1:
                            call("workspace", "1")

                        time.sleep(1.5)
                        with Image.open(io.BytesIO(call("screenshot"))) as image:
                            strip = image.convert("RGB").crop((0, round(16 * scale), image.width, round(strip_bottom)))
                            colors = {"red": 0, "blue": 0, "green": 0}
                            for count, (red, green, blue) in strip.getcolors(strip.width * strip.height):
                                if red > 180 and green < 60 and blue < 60:
                                    colors["red"] += count
                                if blue > 180 and red < 60 and green < 60:
                                    colors["blue"] += count
                                if green > 180 and red < 60 and blue < 60:
                                    colors["green"] += count

                        with self.subTest(active_workspace=workspace):
                            for color, count in colors.items():
                                self.assertGreater(count, 30, f"Missing {color} thumbnail pixels: {colors}")
                finally:
                    for child in reversed(children):
                        if child.poll() is None:
                            os.killpg(child.pid, signal.SIGTERM)
                            child.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
