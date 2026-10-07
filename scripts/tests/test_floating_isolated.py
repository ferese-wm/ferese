"""Opt-in rendered regression test; opens only a temporary nested compositor.
FERESE_TEST_FLOATING=1 FERESE_TEST_BINARY=target/debug/ferese python3 scripts/tests/test_floating_isolated.py
Requires cc, pkg-config, wayland-scanner, wayland-protocols, and Pillow.
The compositor and feresectl must be built; set FERESE_TEST_BINARY to point at
the compositor and FERESE_TEST_CTL at feresectl when they are not siblings.
"""
import json
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import subprocess
import tempfile
import time
import unittest


@unittest.skipUnless(os.environ.get("FERESE_TEST_FLOATING") == "1", "nested render test is opt-in")
class FloatingSizeTest(unittest.TestCase):
    def test_committed_content_is_not_cropped_to_requested_size(self):
        from PIL import Image, ImageChops
        repo = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="ferese-floating-test-") as tmp:
            root = Path(tmp)
            protocols = subprocess.check_output(
                ["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True
            ).strip()
            xml = str(Path(protocols) / "stable/xdg-shell/xdg-shell.xml")
            for kind, output in [("client-header", "xdg-shell-client-protocol.h"),
                                 ("private-code", "xdg-shell-protocol.c")]:
                subprocess.run(["wayland-scanner", kind, xml, str(root / output)], check=True)
            flags = subprocess.check_output(["pkg-config", "--cflags", "--libs", "wayland-client"], text=True).split()
            subprocess.run(["cc", "-Wall", "-Wextra", "-I", str(root),
                            str(repo / "scripts/tests/fixtures/floating-size.c"),
                            str(root / "xdg-shell-protocol.c"), "-o", str(root / "client"), *flags], check=True)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            (config / "config.kdl").write_text(
                'window-rule app-id="ferese.test.floating-size" floating=#true width=320 height=240\n'
                'theme {\n geometry {\n border-width 0\n focus-ring-width 0\n window-radius 0\n }\n}\n'
            )
            display = os.environ["WAYLAND_DISPLAY"]
            if not display.startswith("/"):
                display = str(Path(os.environ["XDG_RUNTIME_DIR"]) / display)
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(root / "config"),
                       WAYLAND_DISPLAY=display, FERESE_ENABLE_SCREENCOPY="1")
            binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/debug/ferese")
            ctl = repo / os.environ.get("FERESE_TEST_CTL", str(binary.parent / "feresectl"))
            if not ctl.is_file():
                self.fail(f"missing {ctl}; build it with cargo build -p feresectl")
            with (root / "compositor.log").open("w") as log:
                process = subprocess.Popen([str(binary), "--backend", "nested", "--", str(root / "client")],
                                           env=ipc_environment(env), stdout=log, stderr=subprocess.STDOUT)
                try:
                    for _ in range(100):
                        if process.poll() is not None:
                            self.fail((root / "compositor.log").read_text())
                        sockets = [p for p in runtime.glob("wayland-*") if not p.name.endswith(".lock")]
                        if sockets:
                            break
                        time.sleep(.1)
                    self.assertTrue(sockets, "nested Wayland socket did not appear")
                    childenv = dict(env, WAYLAND_DISPLAY=str(sockets[0]))
                    time.sleep(2)
                    listed = subprocess.run([str(ctl), "-j", "outputs"], env=ipc_environment(childenv),
                                            check=True, capture_output=True, timeout=10, text=True)
                    enabled = [o for o in json.loads(listed.stdout) if o.get("enabled")]
                    if not enabled:
                        self.fail("compositor reported no enabled output to capture")
                    first = enabled[0]
                    if any(first.get(k) is None for k in ("x", "y", "width", "height")):
                        self.fail(f"enabled output {first.get('name')!r} has no logical geometry")
                    geometry = "%d,%d %dx%d" % (first["x"], first["y"], first["width"], first["height"])
                    screenshot = root / "frame.png"
                    with screenshot.open("wb") as png:
                        captured = subprocess.run([str(ctl), "screenshot", "-g", geometry],
                                                  env=ipc_environment(childenv), stdout=png, stderr=subprocess.PIPE, timeout=15)
                    if captured.returncode != 0:
                        self.fail(f"capture of {geometry} failed: {captured.stderr.decode()}")
                    if screenshot.stat().st_size == 0:
                        self.fail(f"capture of {geometry} produced an empty file")
                    with Image.open(screenshot) as frame:
                        rgb = frame.convert("RGB")
                        red, green, blue = rgb.split()
                        mask = ImageChops.multiply(
                            ImageChops.multiply(red.point(lambda v: 255 if v < 60 else 0),
                                                green.point(lambda v: 255 if v > 180 else 0)),
                            blue.point(lambda v: 255 if v < 100 else 0),
                        )
                        bounds = mask.getbbox()
                    self.assertIsNotNone(bounds, "client content was not rendered")
                    # The shader antialiases the outermost pixel even at radius zero.
                    scale = first["scale"]
                    expected_size = (round(640 * scale), round(480 * scale))
                    for actual, expected in zip((bounds[2] - bounds[0], bounds[3] - bounds[1]), expected_size):
                        self.assertTrue(expected - 2 <= actual <= expected,
                                        f"floating content cropped: {bounds}, expected {expected_size}")
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()


if __name__ == "__main__":
    unittest.main()
