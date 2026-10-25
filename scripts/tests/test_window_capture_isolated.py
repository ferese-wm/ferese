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
import re
from session_socket import ipc_environment, ipc_socket
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
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"), WAYLAND_DISPLAY=str(display), FERESE_ENABLE_SCREENCOPY="1", FERESE_TRACE_PERFORMANCE="1")
            env.pop("WAYLAND_SOCKET", None)
            env.pop("FERESE_SHELL_CONTROL_SOCKET", None)
            log = (root / "compositor.log").open("w")
            try:
                compositor = subprocess.Popen([str(COMPOSITOR), "--backend=nested"], env=ipc_environment(env), stdout=log, stderr=log, start_new_session=True)
                processes.append(compositor)
                deadline = time.monotonic() + 10
                while not (ipc_socket(runtime)).exists():
                    if compositor.poll() is not None or time.monotonic() > deadline:
                        self.fail((root / "compositor.log").read_text())
                    time.sleep(0.02)
                sockets = list(runtime.glob("wayland-*"))
                env["WAYLAND_DISPLAY"] = str(next(path for path in sockets if not path.name.endswith(".lock")))

                def call(*args):
                    return subprocess.check_output([str(CTL), "-j", *args], env=ipc_environment(env), timeout=10)

                def windows(count, width=640):
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        result = json.loads(call("get-windows"))
                        if len(result) == count and all(window["mapped"] and (width is None or window["width"] == width) for window in result):
                            return result
                        time.sleep(0.02)
                    self.fail((root / "compositor.log").read_text())

                target_client = subprocess.Popen([str(root / "client"), "interactive"], env=ipc_environment(env), stdin=subprocess.PIPE, stdout=log, stderr=log, start_new_session=True)
                processes.append(target_client)
                target = windows(1)[0]
                cover_client = subprocess.Popen([str(root / "client"), "cover", "interactive"], env=ipc_environment(env), stdin=subprocess.PIPE, stdout=log, stderr=log, start_new_session=True)
                processes.append(cover_client)
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
                # Capture an inactive workspace, where display repaint callbacks
                # cannot conceal a stalled capture-driven animation.
                call("workspace", "2")
                # Verify real streaming updates and silence between commits. Each
                # requested frame changes a pixel; static content must not be copied
                # repeatedly just to satisfy a consumer's process callbacks.
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
                transport = os.environ.get("FERESE_TEST_CAPTURE_TRANSPORT")
                if transport:
                    flags = subprocess.check_output(["pkg-config", "--cflags", "--libs", "libpipewire-0.3"], text=True).split()
                    subprocess.run(["cc", "-Wall", "-Wextra", str(REPO / "scripts/tests/fixtures/capture-consumer.c"), "-o", str(root / "consumer"), *flags], check=True)
                    command = [str(root / "consumer"), str(ready["node"]), transport, "15", str(raw)]
                else:
                    command = ["gst-launch-1.0", "-q", "pipewiresrc", "min-buffers=4", f'path={ready["node"]}',
                               "num-buffers=15", "!", "video/x-raw,format=BGRx", "!", "filesink", "buffer-mode=unbuffered", f"location={raw}"]
                consumer = subprocess.Popen(command, env=stream_env, stdout=log, stderr=log, start_new_session=True)
                processes.append(consumer)
                frame_bytes = ready["width"] * ready["height"] * 4

                def wait_frames(count):
                    deadline = time.monotonic() + 7
                    while not raw.exists() or raw.stat().st_size < count * frame_bytes:
                        if consumer.poll() is not None or worker.poll() is not None or time.monotonic() >= deadline:
                            self.fail((root / "compositor.log").read_text())
                        time.sleep(0.01)

                wait_frames(1)
                time.sleep(0.35)
                self.assertEqual(raw.stat().st_size, frame_bytes, "unchanged window produced duplicate frames")
                target_client.stdin.write(b"a")
                target_client.stdin.flush()
                wait_frames(4)
                for count in range(5, 16):
                    target_client.stdin.write(b"d")
                    target_client.stdin.flush()
                    wait_frames(count)
                consumer.wait(timeout=7)
                self.assertEqual(consumer.returncode, 0)
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
                call("workspace", "1")
                # A new consumer must receive an initial image even when the
                # source has stayed unchanged since the previous consumer left.
                original_command = command.copy()
                raw = root / "reconnected.bgrx"
                if transport:
                    command[3] = "1"
                    command[-1] = str(raw)
                else:
                    command[command.index("num-buffers=15")] = "num-buffers=1"
                    command[-1] = f"location={raw}"
                consumer = subprocess.Popen(command, env=stream_env, stdout=log, stderr=log, start_new_session=True)
                processes.append(consumer)
                wait_frames(1)
                consumer.wait(timeout=7)
                self.assertEqual(consumer.returncode, 0)
                self.assertEqual(raw.stat().st_size, frame_bytes)
                command = original_command
                # Resize while the consumer is disconnected, then reconnect to
                # the same node. Retired pool buffers must never be queued again.
                resize_width, resize_height = ((481, 321) if transport else (480, 320))
                target_client.stdin.write(b"o" if transport else b"r")
                target_client.stdin.flush()
                deadline = time.monotonic() + 7
                while not any(w["id"] == target["id"] and w["width"] == resize_width for w in json.loads(call("windows"))):
                    if time.monotonic() >= deadline:
                        self.fail("Window did not resize")
                    time.sleep(.02)
                raw = root / "resized.bgrx"
                if transport:
                    command[-1] = str(raw)
                else:
                    command[-1] = f"location={raw}"
                consumer = subprocess.Popen(command, env=stream_env, stdout=log, stderr=log, start_new_session=True)
                processes.append(consumer)
                width, height = round(resize_width * scale), round(resize_height * scale)
                frame_bytes = width * height * 4
                wait_frames(1)
                time.sleep(.35)
                self.assertEqual(raw.stat().st_size, frame_bytes, "paused resize left a stale frame in the stream")
                for count in range(2, 16):
                    target_client.stdin.write(b"d")
                    target_client.stdin.flush()
                    wait_frames(count)
                consumer.wait(timeout=7)
                self.assertEqual(consumer.returncode, 0)
                resized = raw.read_bytes()
                self.assertEqual(len(resized), frame_bytes * 15)
                for index in (0, 14):
                    red = index * frame_bytes + ((height // 4) * width + width // 2) * 4
                    blue = index * frame_bytes + ((height * 3 // 4) * width + width // 2) * 4
                    self.assertEqual(resized[red:red + 3], bytes([0, 0, 255]))
                    self.assertEqual(resized[blue:blue + 3], bytes([255, 0, 0]))
                # A live rule change revokes the window stream and synchronous
                # window screenshot route, while normal display mapping remains.
                config.write_text('window-rule app-id="ferese.test.window-capture" floating=#true\n'
                                  'window-rule title="Capture target" block-out-from-screencasts=#true\n')
                call("reload-config")
                self.assertEqual(worker.wait(timeout=7), 0)
                worker.stdin.close()
                worker.stdout.close()
                rejected = subprocess.run([str(CTL), "screenshot-window", str(target["id"])],
                                          env=ipc_environment(env), capture_output=True, timeout=10)
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn(b"protected", rejected.stderr)
                self.assertTrue(next(window for window in windows(2, width=None) if window["id"] == target["id"])["mapped"])
                with Image.open(io.BytesIO(call("screenshot-window", str(cover["id"])))) as image:
                    self.assertEqual(image.convert("RGB").getpixel((image.width // 2, image.height // 2)), (0, 255, 0))
                os.killpg(processes[2].pid, signal.SIGTERM)
                processes[2].wait(timeout=5)
                protected = windows(1, width=resize_width)[0]
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
                # Updates behind an opaque private window still belong in the
                # filtered capture, even when display occlusion hides them.
                config.write_text('window-rule app-id="ferese.test.window-capture" floating=#true\n'
                                  'window-rule title="Occluding window" block-out-from-screencasts=#true\n')
                call("reload-config")
                private_cover = subprocess.Popen([str(root / "client"), "cover", "interactive"],
                                                 env=ipc_environment(env), stdin=subprocess.PIPE,
                                                 stdout=log, stderr=log, start_new_session=True)
                processes.append(private_cover)
                pair = windows(2, width=None)
                cover = next(window for window in pair if window["title"] == "Occluding window")
                target = next(window for window in pair if window["id"] == target["id"])
                marker_x = target["x"] + target["width"] / 2
                marker_y = target["y"] + target["height"] / 2
                self.assertLess(cover["x"], marker_x)
                self.assertGreater(cover["x"] + cover["width"], marker_x)
                self.assertLess(cover["y"], marker_y)
                self.assertGreater(cover["y"] + cover["height"], marker_y)
                # Monitor capture uses an independent filtered scene and the same
                # buffer transport. It must also wait quietly between real updates.
                # Adding the occluder and reloading rules can animate focus
                # borders. Those are real damage, so establish a settled scene
                # before asserting that an unchanged monitor produces no frames.
                previous, stable_since = None, time.monotonic()
                deadline = time.monotonic() + 7
                while time.monotonic() < deadline:
                    image = call("screenshot")
                    if image != previous:
                        previous, stable_since = image, time.monotonic()
                    elif time.monotonic() - stable_since >= .35:
                        break
                    time.sleep(.04)
                else:
                    self.fail("Monitor did not settle before the damage-waiting check")
                sources = json.loads(subprocess.check_output([str(PORTAL), "--sources"], env=stream_env, timeout=7))
                worker = subprocess.Popen([str(PORTAL), "--stream", sources[0]["name"], "hidden"], env=stream_env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, start_new_session=True)
                processes.append(worker)
                with selectors.DefaultSelector() as selector:
                    selector.register(worker.stdout, selectors.EVENT_READ)
                    self.assertTrue(selector.select(10), log.name)
                ready = json.loads(worker.stdout.readline())
                raw = root / "monitor.bgrx"
                frame_bytes = ready["width"] * ready["height"] * 4
                if transport:
                    command = [str(root / "consumer"), str(ready["node"]), transport, "15", str(raw)]
                else:
                    command = ["gst-launch-1.0", "-q", "pipewiresrc", f'path={ready["node"]}', "num-buffers=15", "!",
                               "video/x-raw,format=BGRx", "!", "filesink", "buffer-mode=unbuffered", f"location={raw}"]
                consumer = subprocess.Popen(command, env=stream_env, stdout=log, stderr=log, start_new_session=True)
                processes.append(consumer)
                wait_frames(1)
                # Wait for a real compositor report, rather than inferring GPU
                # reuse merely from the absence of delivered duplicate frames.
                deadline = time.monotonic() + 7
                while time.monotonic() < deadline:
                    reports = [line for line in (root / "compositor.log").read_text().splitlines()
                               if "capture scene outcomes" in line and "include_cursor=false" in line]
                    if any(int(match.group(1)) > 0 for line in reports
                           if (match := re.search(r"scene_reuses=(\d+)", line))):
                        break
                    time.sleep(.05)
                else:
                    self.fail("Unchanged capture never reported reusing its filtered scene:\n"
                              + (root / "compositor.log").read_text())
                monitor_idle = raw.read_bytes()
                self.assertEqual(len(monitor_idle), frame_bytes,
                                 "unchanged monitor produced extra frames; first two images equal="
                                 + str(monitor_idle[:frame_bytes] == monitor_idle[frame_bytes:2 * frame_bytes]))
                for count in range(2, 16):
                    target_client.stdin.write(b"d")
                    target_client.stdin.flush()
                    wait_frames(count)
                consumer.wait(timeout=7)
                self.assertEqual(consumer.returncode, 0)
                monitor = raw.read_bytes()
                self.assertEqual(len(monitor), frame_bytes * 15)
                probe_x = round((cover["x"] + cover["width"] / 2) * scale)
                probe_y = round((cover["y"] + cover["height"] / 4) * scale)
                probe = (probe_y * ready["width"] + probe_x) * 4
                for index in (0, 14):
                    start = index * frame_bytes + probe
                    self.assertNotEqual(monitor[start:start + 3], bytes([0, 255, 0]),
                                        "private occluder leaked into the filtered capture")
                for index in range(1, 15):
                    self.assertTrue(
                        monitor[(index - 1) * frame_bytes:index * frame_bytes]
                        != monitor[index * frame_bytes:(index + 1) * frame_bytes],
                        f"monitor stream reused stale pixels after source damage at frame {index}")
                worker.stdin.close()
                self.assertEqual(worker.wait(timeout=7), 0)
            finally:
                for process in reversed(processes):
                    if process.poll() is None:
                        os.killpg(process.pid, signal.SIGTERM)
                        process.wait(timeout=5)
                    if process.stdin:
                        process.stdin.close()
                    if process.stdout and process.stdout is not log:
                        process.stdout.close()
                log.close()


if __name__ == "__main__":
    unittest.main()
