"""Exercise live isolated window sharing in a disposable nested compositor.

FERESE_TEST_WINDOW_STREAM=1 python3 scripts/tests/test_window_stream_isolated.py
Requires a Wayland host, PipeWire, GStreamer Python bindings and a C compiler.
Only this test's compositor, windows and streams are stopped.
"""
import json
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import select
import signal
import subprocess
import tempfile
import time
import unittest

from test_inhibit_isolated import IPC

REPO = Path(__file__).resolve().parents[2]


@unittest.skipUnless(os.environ.get("FERESE_TEST_WINDOW_STREAM") == "1", "requires Wayland and PipeWire")
class WindowStream(unittest.TestCase):
    def test_isolation_resize_workspace_and_close(self):
        import gi
        gi.require_version("Gst", "1.0")
        from gi.repository import Gst
        Gst.init(None)
        processes = []
        pipeline = None
        admin = None

        def wait_for(check, message):
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                value = check()
                if value:
                    return value
                time.sleep(0.02)
            self.fail(message)

        with tempfile.TemporaryDirectory(prefix="ferese-window-stream-") as directory:
            root = Path(directory)
            protocols = subprocess.check_output(["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True).strip()
            xml = str(Path(protocols) / "stable/xdg-shell/xdg-shell.xml")
            for kind, output in [("client-header", "xdg-shell-client-protocol.h"), ("private-code", "xdg-shell-protocol.c")]:
                subprocess.run(["wayland-scanner", kind, xml, str(root / output)], check=True)
            flags = subprocess.check_output(["pkg-config", "--cflags", "--libs", "wayland-client"], text=True).split()
            subprocess.run(["cc", "-Wall", "-Wextra", "-I", str(root), str(REPO / "scripts/tests/fixtures/window-capture.c"), str(root / "xdg-shell-protocol.c"), "-o", str(root / "client"), *flags], check=True)
            lock_xml = str(Path(protocols) / "staging/ext-session-lock/ext-session-lock-v1.xml")
            for kind, output in [("client-header", "ext-session-lock-client-protocol.h"), ("private-code", "ext-session-lock-protocol.c")]:
                subprocess.run(["wayland-scanner", kind, lock_xml, str(root / output)], check=True)
            subprocess.run(["cc", "-Wall", "-Wextra", "-I", str(root), str(REPO / "scripts/tests/fixtures/session-lock.c"), str(root / "ext-session-lock-protocol.c"), "-o", str(root / "locker"), *flags], check=True)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese/config.kdl"
            config.parent.mkdir(parents=True)
            config.write_text('window-rule app-id="ferese.test.window-capture" floating=#true\nstatus { keybinding-guide #false; }\n')
            host_runtime = os.environ["XDG_RUNTIME_DIR"]
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(host_runtime) / display
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"), WAYLAND_DISPLAY=str(display), PIPEWIRE_RUNTIME_DIR=host_runtime, FERESE_ENABLE_SCREENCOPY="1")
            env.pop("WAYLAND_SOCKET", None)
            env.pop("FERESE_SHELL_CONTROL_SOCKET", None)
            log = (root / "test.log").open("w")
            try:
                compositor = subprocess.Popen([str(REPO / "target/debug/ferese"), "--backend=nested"], env=ipc_environment(env), stdout=log, stderr=log, start_new_session=True)
                processes.append(compositor)
                wait_for(lambda: ipc_socket(runtime).is_socket(), "Nested compositor did not start")
                socket = ipc_socket(runtime)
                admin = IPC(socket)
                sockets = [path for path in runtime.glob("wayland-*") if not path.name.endswith(".lock")]
                env["WAYLAND_DISPLAY"] = str(sockets[0])
                target = subprocess.Popen([str(root / "client"), "resize"], env=ipc_environment(env), stdin=subprocess.PIPE, stdout=log, stderr=log, start_new_session=True)
                processes.append(target)
                windows = wait_for(lambda: [window for window in admin.call("get-windows") if window["capture_width"] == 640], "Window fixture did not map; private manager must be hidden from ordinary clients")
                target_id = windows[0]["id"]
                cover = subprocess.Popen([str(root / "client"), "cover"], env=ipc_environment(env), stdout=log, stderr=log, start_new_session=True)
                processes.append(cover)
                wait_for(lambda: len(admin.call("get-windows")) == 2, "Occluding window did not map")
                helper = subprocess.Popen([str(REPO / "target/debug/xdg-desktop-portal-ferese"), "--stream-window", str(target_id), "hidden"], env=ipc_environment(env), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, text=True, start_new_session=True)
                processes.append(helper)
                self.assertTrue(select.select([helper.stdout], [], [], 10)[0], "Stream did not announce a PipeWire node")
                ready = json.loads(helper.stdout.readline())
                self.assertEqual(ready["logical_size"], [640, 480])
                pipeline = Gst.parse_launch(f'pipewiresrc path={ready["node"]} ! video/x-raw,format=BGRx ! appsink name=frames sync=false max-buffers=1 drop=true')
                sink = pipeline.get_by_name("frames")
                pipeline.set_state(Gst.State.PLAYING)

                def frame(expected=None):
                    deadline = time.monotonic() + 8
                    while time.monotonic() < deadline:
                        sample = sink.emit("try-pull-sample", Gst.SECOND)
                        if sample is None:
                            continue
                        caps = sample.get_caps().get_structure(0)
                        size = (caps.get_value("width"), caps.get_value("height"))
                        if expected and size != expected:
                            continue
                        buffer = sample.get_buffer()
                        pixels = buffer.extract_dup(0, buffer.get_size())
                        width, height = size
                        self.assertEqual(tuple(pixels[((height // 4) * width + width // 2) * 4:][:3]), (0, 0, 255))
                        self.assertEqual(tuple(pixels[((height * 3 // 4) * width + width // 2) * 4:][:3]), (255, 0, 0))
                        return size
                    message = pipeline.get_bus().pop_filtered(Gst.MessageType.ERROR)
                    self.fail(f"Missing expected frame {expected}; pipeline error: {message.parse_error() if message else None}")

                original = frame()
                self.assertEqual(original, (ready["width"], ready["height"]))
                admin.call("workspace", {"index": 2})
                time.sleep(0.4)
                frame(original)
                self.assertIsNone(helper.poll(), "Inactive workspace stopped the stream")
                target.stdin.write(b"r")
                target.stdin.flush()
                scale = ready["width"] / 640
                resized = (round(480 * scale), round(320 * scale))
                frame(resized)
                self.assertIsNone(helper.poll(), "Resize stopped the stream")
                target.terminate()
                target.wait(timeout=3)
                wait_for(lambda: helper.poll() is not None, "Closing the selected window left capture running")
                self.assertIsNone(cover.poll(), "Unrelated window was closed")
                pipeline.set_state(Gst.State.NULL)
                pipeline = None
                cover_id = next(window["id"] for window in admin.call("get-windows") if window["title"] == "Occluding window")
                paused = subprocess.Popen([str(REPO / "target/debug/xdg-desktop-portal-ferese"), "--stream-window", str(cover_id), "hidden"], env=ipc_environment(env), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, text=True, start_new_session=True)
                processes.append(paused)
                self.assertTrue(select.select([paused.stdout], [], [], 10)[0])
                self.assertGreater(json.loads(paused.stdout.readline())["width"], 0)
                locker = subprocess.Popen([str(root / "locker")], env=ipc_environment(env), stdout=subprocess.PIPE, stderr=log, text=True, start_new_session=True)
                processes.append(locker)
                self.assertTrue(select.select([locker.stdout], [], [], 5)[0], "Nested lock request did not complete")
                self.assertIn(locker.stdout.readline().strip(), ("locked", "requested"))
                wait_for(lambda: paused.poll() is not None, "Lock did not end a paused window stream")
                print("PASS: isolated pixels, private manager, upright video, inactive workspace, resize renegotiation, closure and paused lock revocation")
            except Exception:
                print((root / "test.log").read_text())
                raise
            finally:
                if pipeline:
                    pipeline.set_state(Gst.State.NULL)
                if admin:
                    admin.close()
                for process in reversed(processes):
                    if process.poll() is None:
                        os.killpg(process.pid, signal.SIGTERM)
                        try:
                            process.wait(timeout=3)
                        except subprocess.TimeoutExpired:
                            os.killpg(process.pid, signal.SIGKILL)
                            process.wait()
                for process in processes:
                    for pipe in (process.stdin, process.stdout, process.stderr):
                        if pipe:
                            pipe.close()
                log.close()


if __name__ == "__main__":
    unittest.main()
