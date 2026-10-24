"""Keep native backing pixels in window streams on a fractionally scaled host.

FERESE_TEST_WINDOW_CAPTURE_SCALE=1 python3 scripts/tests/test_window_capture_scale_isolated.py
Requires a Wayland host, PipeWire, GStreamer, wayland-scanner, cc, and Pillow.
"""
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile
import time
import unittest

from session_socket import ipc_environment, ipc_socket

REPO = Path(__file__).resolve().parents[2]
COMPOSITOR = Path(os.environ.get("FERESE_TEST_BINARY", REPO / "target/release/ferese"))
CTL = Path(os.environ.get("FERESE_TEST_CTL", REPO / "target/release/feresectl"))
PORTAL = Path(os.environ.get("FERESE_TEST_PORTAL", REPO / "target/release/xdg-desktop-portal-ferese"))


@unittest.skipUnless(os.environ.get("FERESE_TEST_WINDOW_CAPTURE_SCALE") == "1", "opt-in capture test")
class WindowCaptureScale(unittest.TestCase):
    def test_backing_pixels_survive_fractional_output_scale(self):
        children = []
        with tempfile.TemporaryDirectory(prefix="ferese-capture-scale-") as temporary:
            root = Path(temporary)
            protocols = Path(subprocess.check_output(
                ["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True).strip())
            sources = []
            for protocol, relative in [("xdg-shell", "stable/xdg-shell/xdg-shell.xml"),
                                       ("viewporter", "stable/viewporter/viewporter.xml")]:
                source = root / f"{protocol}-protocol.c"
                subprocess.run(["wayland-scanner", "client-header", str(protocols / relative),
                                str(root / f"{protocol}-client-protocol.h")], check=True)
                subprocess.run(["wayland-scanner", "private-code", str(protocols / relative),
                                str(source)], check=True)
                sources.append(str(source))
            flags = subprocess.check_output(
                ["pkg-config", "--cflags", "--libs", "wayland-client"], text=True).split()
            subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-I", str(root),
                            str(REPO / "scripts/tests/fixtures/window-capture-scale.c"),
                            *sources, "-o", str(root / "client"), *flags], check=True)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            (config / "config.kdl").write_text(
                'animations { reduced-motion #true; }\n'
                'window-rule app-id="ferese.test.capture-scale" floating=#true\n')
            host_runtime = Path(os.environ["XDG_RUNTIME_DIR"])
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = host_runtime / display
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=str(display),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       XDG_DATA_HOME=str(root / "data"), FERESE_ENABLE_SCREENCOPY="1",
                       PIPEWIRE_REMOTE=str(host_runtime / "pipewire-0"))
            for key in ("WAYLAND_SOCKET", "FERESE_SHELL_CONTROL_SOCKET", "DISPLAY", "XAUTHORITY"):
                env.pop(key, None)
            log_path = root / "session.log"
            with log_path.open("w") as log:
                def launch(command, **kwargs):
                    child = subprocess.Popen(command, env=ipc_environment(env), stderr=log,
                                             stdout=kwargs.pop("stdout", log),
                                             start_new_session=True, **kwargs)
                    children.append(child)
                    return child

                def wait_for(check):
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        result = check()
                        if result:
                            return result
                        if compositor.poll() is not None:
                            self.fail(log_path.read_text())
                        time.sleep(.03)
                    self.fail("Timed out:\n" + log_path.read_text())

                def call(*args):
                    return json.loads(subprocess.check_output(
                        [str(CTL), "-j", *args], env=ipc_environment(env), timeout=5))

                try:
                    compositor = launch([str(COMPOSITOR), "--backend=nested"])
                    wait_for(lambda: ipc_socket(runtime).is_socket())
                    env["WAYLAND_DISPLAY"] = str(wait_for(
                        lambda: next((p for p in runtime.glob("wayland-*") if p.is_socket()), None)))
                    scale = call("outputs")[0]["scale"]
                    print(f"Isolated capture output scale: {scale}", flush=True)
                    self.assertTrue(1 < scale < 2,
                                    "This regression needs a fractional host scale between 1 and 2; "
                                    "an integer 2x host masks the original downsampling bug")
                    for mode, density in [("integer", 2), ("viewport", 2),
                                          ("fractional", 1.75), ("cropped", 1),
                                          ("subsurface", 2), ("subsurface-outside", 1),
                                          ("fractional-rounded", 1.75),
                                          ("fractional-ceiled", 1.75)]:
                        with self.subTest(mode=mode, output_scale=scale):
                            client = launch([str(root / "client"), mode], stdin=subprocess.PIPE)
                            window = wait_for(lambda: next((w for w in call("windows")
                                                           if w["title"] == mode and w["mapped"]), None))
                            worker = launch([str(PORTAL), "--stream-window", str(window["id"]), "hidden"],
                                            stdin=subprocess.PIPE, stdout=subprocess.PIPE)
                            with selectors.DefaultSelector() as selector:
                                selector.register(worker.stdout, selectors.EVENT_READ)
                                self.assertTrue(selector.select(10), log_path.read_text())
                            ready = json.loads(worker.stdout.readline())
                            expected = (round(640 * max(scale, density)),
                                        round(480 * max(scale, density)))
                            if mode == "fractional-rounded":
                                expected = (3819, 2091)
                            elif mode == "fractional-ceiled":
                                expected = (3819, 2092)
                            self.assertEqual((ready["width"], ready["height"]), expected,
                                             "window sharing discarded native backing pixels")
                            raw = root / f"{mode}.bgrx"
                            consumer = launch(["gst-launch-1.0", "-q", "pipewiresrc",
                                               f'path={ready["node"]}', "num-buffers=3", "!",
                                               "video/x-raw,format=BGRx", "!", "filesink", "buffer-mode=unbuffered", f"location={raw}"])
                            frame_bytes = expected[0] * expected[1] * 4
                            wait_for(lambda: raw.exists() and raw.stat().st_size >= frame_bytes)
                            # A static source waits for damage. Generate each next
                            # commit explicitly instead of relying on duplicate frames.
                            for count in (2, 3):
                                client.stdin.write(b"d")
                                client.stdin.flush()
                                wait_for(lambda: raw.stat().st_size >= count * frame_bytes)
                            consumer.wait(timeout=5)
                            self.assertEqual(consumer.returncode, 0)
                            width, height = expected
                            pixels = raw.read_bytes()
                            self.assertEqual(len(pixels), width * height * 4 * 3)
                            if mode == "subsurface-outside":
                                self.assertFalse(any(pixels[0::4]) or any(pixels[1::4])
                                                 or any(pixels[2::4]),
                                                 "content outside the window was captured")
                            elif density >= scale:
                                for index in (0, 2):
                                    if mode.startswith("fractional-"):
                                        for y in (0, height // 2, height - 1):
                                            start = (index * width * height + y * width) * 4
                                            row = pixels[start:start + width * 4]
                                            expected_row = b"".join(
                                                bytes((255, 255, 255, 255)) if (x + y) % 2
                                                else bytes((0, 0, 0, 255))
                                                for x in range(width))
                                            self.assertEqual(row, expected_row,
                                                             "rounded native pixels were filtered")
                                        continue
                                    start = (index * width * height + height // 2 * width) * 4
                                    row = pixels[start:start + width * 4]
                                    self.assertEqual(row, bytes((0, 0, 0, 255, 255, 255, 255, 255)) * (width // 2),
                                                     "native one-pixel stripes were filtered")
                            worker.stdin.close()
                            worker.wait(timeout=5)
                            worker.stdout.close()
                            client.terminate()
                            client.wait(timeout=5)
                            client.stdin.close()
                            wait_for(lambda: not any(w["id"] == window["id"] for w in call("windows")))
                finally:
                    for child in reversed(children):
                        if child.poll() is None:
                            os.killpg(child.pid, signal.SIGTERM)
                            child.wait(timeout=5)
                        for pipe in (child.stdin, child.stdout, child.stderr):
                            if pipe:
                                pipe.close()


if __name__ == "__main__":
    unittest.main()
