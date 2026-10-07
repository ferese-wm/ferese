"""Verify isolated window capture without exposing host desktop content.

FERESE_TEST_WINDOW_CAPTURE=1 python3 scripts/tests/test_window_capture_isolated.py
Requires built debug binaries, a Wayland host, PipeWire, gst-launch-1.0,
cc, wayland-scanner and Pillow.
Override FERESE_TEST_BINARY, FERESE_TEST_CTL and FERESE_TEST_PORTAL to test a release build.
"""
import io
import json
import os
from pathlib import Path
import signal
import selectors
import subprocess
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[2]
COMPOSITOR = Path(os.environ.get("FERESE_TEST_BINARY", REPO / "target/debug/ferese"))
CTL = Path(os.environ.get("FERESE_TEST_CTL", REPO / "target/debug/feresectl"))
PORTAL = Path(os.environ.get("FERESE_TEST_PORTAL", REPO / "target/debug/xdg-desktop-portal-ferese"))


@unittest.skipUnless(os.environ.get("FERESE_TEST_WINDOW_CAPTURE") == "1", "requires a Wayland host")
class WindowCapture(unittest.TestCase):
    def test_occluding_window_does_not_appear_and_rows_keep_orientation(self):
        from PIL import Image
        processes = []
        with tempfile.TemporaryDirectory(prefix="ferese-window-capture-") as directory:
            root = Path(directory)
            protocols = subprocess.check_output(["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True).strip()
            xml = str(Path(protocols) / "stable/xdg-shell/xdg-shell.xml")
            for kind, output in [("client-header", "xdg-shell-client-protocol.h"), ("private-code", "xdg-shell-protocol.c")]:
                subprocess.run(["wayland-scanner", kind, xml, str(root / output)], check=True)
            flags = subprocess.check_output(["pkg-config", "--cflags", "--libs", "wayland-client"], text=True).split()
            subprocess.run(["cc", "-Wall", "-Wextra", "-I", str(root), str(REPO / "scripts/tests/fixtures/window-capture.c"),
                            str(root / "xdg-shell-protocol.c"), "-o", str(root / "client"), *flags], check=True)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese/config.kdl"
            config.parent.mkdir(parents=True)
            config.write_text('window-rule app-id="ferese.test.window-capture" floating=#true\n')
            # Remembered geometry deliberately places both clients together;
            # ordinary placement now avoids overlap. Keep host state isolated.
            state = root / "state/ferese"
            state.mkdir(parents=True)
            (state / "floating.json").write_text(json.dumps({
                "ferese.test.window-capture": {
                    "output": "ferese-winit", "fractions": [.1, .1, .4, .4],
                },
            }))
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"), WAYLAND_DISPLAY=str(display), FERESE_ENABLE_SCREENCOPY="1")
            env.pop("WAYLAND_SOCKET", None)
            env.pop("FERESE_SHELL_CONTROL_SOCKET", None)
            log = (root / "compositor.log").open("w")
            try:
                compositor = subprocess.Popen([str(COMPOSITOR), "--backend=nested"], env=env, stdout=log, stderr=log, start_new_session=True)
                processes.append(compositor)
                deadline = time.monotonic() + 10
                while not (runtime / "ferese/control.sock").exists():
                    if compositor.poll() is not None or time.monotonic() > deadline:
                        self.fail((root / "compositor.log").read_text())
                    time.sleep(0.02)
                sockets = list(runtime.glob("wayland-*"))
                env["WAYLAND_DISPLAY"] = str(next(path for path in sockets if not path.name.endswith(".lock")))

                def call(*args):
                    return subprocess.check_output([str(CTL), "-j", *args], env=env, timeout=10)

                def windows(count):
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        result = json.loads(call("get-windows"))
                        if len(result) == count and all(window["mapped"] and window["width"] == 640 for window in result):
                            return result
                        time.sleep(0.02)
                    self.fail((root / "compositor.log").read_text())

                processes.append(subprocess.Popen([str(root / "client")], env=env, stdout=log, stderr=log, start_new_session=True))
                target = windows(1)[0]
                processes.append(subprocess.Popen([str(root / "client"), "cover"], env=env, stdout=log, stderr=log, start_new_session=True))
                current = windows(2)
                cover = next(window for window in current if window["title"] == "Occluding window")
                self.assertEqual((target["x"], target["y"], target["width"], target["height"]),
                                 (cover["x"], cover["y"], cover["width"], cover["height"]))
                scale = json.loads(call("get-outputs"))[0]["scale"]
                with Image.open(io.BytesIO(call("screenshot-window", str(target["id"])))) as image:
                    self.assertEqual(image.size, (round(640 * scale), round(480 * scale)))
                    self.assertEqual(image.convert("RGB").getpixel((image.width // 2, image.height // 4)), (255, 0, 0))
                    self.assertEqual(image.convert("RGB").getpixel((image.width // 2, image.height * 3 // 4)), (0, 0, 255))
                    self.assertEqual(image.convert("RGBA").getpixel((round(40 * scale), round(80 * scale))), (255, 0, 0, 128))
                with Image.open(io.BytesIO(call("screenshot-window", str(cover["id"])))) as image:
                    self.assertEqual(image.convert("RGB").getpixel((image.width // 2, image.height // 2)), (0, 255, 0))
                # Exercise the asynchronous conversion worker and reuse its render
                # target across a stream, not just the synchronous screenshot path.
                stream_env = dict(env, PIPEWIRE_REMOTE=str(Path(os.environ["XDG_RUNTIME_DIR"]) / "pipewire-0"))
                worker = subprocess.Popen([str(PORTAL),
                                           "--stream-window", str(target["id"]), "hidden"],
                                          env=stream_env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                          stderr=log, start_new_session=True)
                processes.append(worker)
                with selectors.DefaultSelector() as selector:
                    selector.register(worker.stdout, selectors.EVENT_READ)
                    self.assertTrue(selector.select(10), "window stream did not publish a node")
                ready = json.loads(worker.stdout.readline())
                self.assertEqual((ready["width"], ready["height"]), (round(640 * scale), round(480 * scale)))
                raw = root / "frames.bgrx"
                subprocess.run(["gst-launch-1.0", "-q", "pipewiresrc", "min-buffers=4", f'path={ready["node"]}',
                                "num-buffers=15", "!", "video/x-raw,format=BGRx", "!", "filesink",
                                f"location={raw}"], env=stream_env, check=True, timeout=20)
                pixels = raw.read_bytes()
                width, height = ready["width"], ready["height"]
                frame_bytes = width * height * 4
                self.assertEqual(len(pixels), frame_bytes * 15)
                for frame_index in (0, 14):
                    start = frame_index * frame_bytes
                    red = start + ((height // 4) * width + width // 2) * 4
                    blue = start + ((height * 3 // 4) * width + width // 2) * 4
                    self.assertEqual(pixels[red:red + 3], bytes([0, 0, 255]))
                    self.assertEqual(pixels[blue:blue + 3], bytes([255, 0, 0]))
                # A live rule change revokes the window stream and synchronous
                # window screenshot route, while normal display mapping remains.
                config.write_text('window-rule app-id="ferese.test.window-capture" floating=#true\n'
                                  'window-rule title="Capture target" block-out-from-screencasts=#true\n')
                call("reload-config")
                self.assertEqual(worker.wait(timeout=7), 0)
                worker.stdin.close()
                worker.stdout.close()
                rejected = subprocess.run([str(CTL), "screenshot-window", str(target["id"])],
                                          env=env, capture_output=True, timeout=10)
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn(b"protected", rejected.stderr)
                self.assertTrue(next(window for window in windows(2) if window["id"] == target["id"])["mapped"])
                with Image.open(io.BytesIO(call("screenshot-window", str(cover["id"])))) as image:
                    self.assertEqual(image.convert("RGB").getpixel((image.width // 2, image.height // 2)), (0, 255, 0))
                os.killpg(processes[2].pid, signal.SIGTERM)
                processes[2].wait(timeout=5)
                protected = windows(1)[0]
                self.assertTrue(protected["mapped"])
                for args in [("screenshot",), ("screenshot", "--geometry",
                             f'{protected["x"]},{protected["y"]} {protected["width"]}x{protected["height"]}')]:
                    with Image.open(io.BytesIO(call(*args))) as image:
                        self.assertFalse(any(pixel in ((255, 0, 0), (0, 0, 255))
                                             for pixel in image.convert("RGB").getdata()))
                # Reusing the capture target after opt-out must produce a fresh frame.
                config.write_text('window-rule app-id="ferese.test.window-capture" floating=#true\n')
                call("reload-config")
                with Image.open(io.BytesIO(call("screenshot-window", str(target["id"])))) as image:
                    self.assertEqual(image.convert("RGB").getpixel((image.width // 2, image.height // 4)), (255, 0, 0))
            finally:
                for process in reversed(processes):
                    if process.poll() is None:
                        os.killpg(process.pid, signal.SIGTERM)
                        process.wait(timeout=5)
                log.close()


if __name__ == "__main__":
    unittest.main()
