"""Exercise workspace sharing on an isolated nested desktop and real PipeWire.

FERESE_TEST_WORKSPACE_CAPTURE=1 python3 scripts/tests/test_workspace_capture_isolated.py
Set FERESE_TEST_CAPTURE_TRANSPORT=dma to require DMA-BUF (default: shm).
Only synthetic test windows are captured; the host desktop is never a source.
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
COMPOSITOR = Path(os.environ.get("FERESE_TEST_BINARY", REPO / "target/debug/ferese"))
CTL = Path(os.environ.get("FERESE_TEST_CTL", REPO / "target/debug/feresectl"))
PORTAL = Path(os.environ.get("FERESE_TEST_PORTAL", REPO / "target/debug/xdg-desktop-portal-ferese"))


@unittest.skipUnless(os.environ.get("FERESE_TEST_WORKSPACE_CAPTURE") == "1", "requires Wayland and PipeWire")
class WorkspaceCapture(unittest.TestCase):
    def test_pinned_workspace_damage_callbacks_privacy_and_resume(self):
        from PIL import Image
        children = []
        with tempfile.TemporaryDirectory(prefix="ferese-workspace-capture-") as temporary:
            root = Path(temporary)
            protocols = Path(subprocess.check_output(
                ["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True).strip())
            xml = protocols / "stable/xdg-shell/xdg-shell.xml"
            for kind, name in [("client-header", "xdg-shell-client-protocol.h"), ("private-code", "xdg-shell-protocol.c")]:
                subprocess.run(["wayland-scanner", kind, str(xml), str(root / name)], check=True)
            for fixture, name, package, extra in [
                ("window-capture.c", "client", "wayland-client", [str(root / "xdg-shell-protocol.c")]),
                ("capture-consumer.c", "consumer", "libpipewire-0.3", []),
            ]:
                flags = subprocess.check_output(["pkg-config", "--cflags", "--libs", package], text=True).split()
                subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-I", str(root),
                                str(REPO / "scripts/tests/fixtures" / fixture), *extra,
                                "-o", str(root / name), *flags], check=True)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese/config.kdl"
            config.parent.mkdir(parents=True)
            base_config = ('animations { reduced-motion #true; }\n'
                           'window-rule app-id="ferese.test.window-capture" floating=#true\n')
            config.write_text(base_config)
            host_runtime = Path(os.environ["XDG_RUNTIME_DIR"])
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = host_runtime / display
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=str(display),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       XDG_CACHE_HOME=str(root / "cache"), FERESE_ENABLE_SCREENCOPY="1",
                       FERESE_TRACE_PERFORMANCE="1", FERESE_TEST_CAPTURE_MARKER_SIZE="16",
                       PIPEWIRE_REMOTE=str(host_runtime / "pipewire-0"))
            for key in ("WAYLAND_SOCKET", "FERESE_SHELL_CONTROL_SOCKET", "DISPLAY", "XAUTHORITY"):
                env.pop(key, None)
            transport = os.environ.get("FERESE_TEST_CAPTURE_TRANSPORT", "shm")
            if transport == "shm":
                env["FERESE_CAPTURE_FORCE_SHM"] = "1"
            else:
                env.pop("FERESE_CAPTURE_FORCE_SHM", None)
            log_path = root / "session.log"
            with log_path.open("w") as log:
                def launch(command, **kwargs):
                    child = subprocess.Popen(command, env=ipc_environment(env), stderr=log,
                                             stdout=kwargs.pop("stdout", log), start_new_session=True, **kwargs)
                    children.append(child)
                    return child

                def until(predicate, message, timeout=10):
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        result = predicate()
                        if result:
                            return result
                        time.sleep(.02)
                    self.fail((message() if callable(message) else message) + "\n" + log_path.read_text())

                def call(*args):
                    return json.loads(subprocess.check_output([str(CTL), "-j", *args], env=ipc_environment(env), timeout=10))

                def change(client, command):
                    client.stdin.write(command)
                    client.stdin.flush()

                try:
                    compositor = launch([str(COMPOSITOR), "--backend=nested"])
                    until(lambda: ipc_socket(runtime).exists(), "nested compositor did not start")
                    env["WAYLAND_DISPLAY"] = str(next(path for path in runtime.glob("wayland-*") if not path.name.endswith(".lock")))
                    target_client = launch([str(root / "client"), "interactive"], stdin=subprocess.PIPE)
                    until(lambda: len(call("windows")) == 1, "target did not map")
                    target = call("windows")[0]
                    workspace = target["workspace"]
                    output = call("outputs")[0]
                    call("workspace", "2")
                    private_client = launch([str(root / "client"), "cover", "interactive"], stdin=subprocess.PIPE)
                    until(lambda: len(call("windows")) == 2, "private workspace did not map")
                    private_workspace = next(window["workspace"] for window in call("windows") if window["id"] != target["id"])

                    def usage(process):
                        fields = Path(f"/proc/{process.pid}/stat").read_text().rsplit(")", 1)[1].split()
                        ticks = int(fields[11]) + int(fields[12])
                        rss = int(fields[21]) * os.sysconf("SC_PAGE_SIZE")
                        return ticks, rss

                    def idle_cpu(processes, seconds=.6):
                        before = [usage(process) for process in processes]
                        start = time.monotonic()
                        time.sleep(seconds)
                        elapsed = time.monotonic() - start
                        after = [usage(process) for process in processes]
                        return [(100 * (end[0] - begin[0]) / os.sysconf("SC_CLK_TCK") / elapsed, end[1])
                                for begin, end in zip(before, after)]

                    time.sleep(1.5)
                    baseline = idle_cpu([compositor], seconds=1.0)[0]

                    def worker():
                        child = launch([str(PORTAL), "--stream-workspace", str(workspace), "hidden"],
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE)
                        with selectors.DefaultSelector() as selector:
                            selector.register(child.stdout, selectors.EVENT_READ)
                            self.assertTrue(selector.select(10), log_path.read_text())
                        ready = json.loads(child.stdout.readline())
                        self.assertEqual(ready["logical_size"], [output["width"], output["height"]])
                        self.assertLessEqual(ready["width"] * ready["height"], 1920 * 1080)
                        return child, ready

                    stream, ready = worker()
                    frame_bytes = ready["width"] * ready["height"] * 4
                    raw = root / "frames.bgrx"

                    def consumer(path, node, frames=1000):
                        return launch([str(root / "consumer"), str(node), transport, str(frames), str(path)])

                    first_consumer = consumer(raw, ready["node"])

                    def count(path=raw):
                        return path.stat().st_size // frame_bytes if path.exists() else 0

                    def image(path=raw):
                        with path.open("rb") as frames:
                            frames.seek((count(path) - 1) * frame_bytes)
                            return Image.frombytes("RGB", (ready["width"], ready["height"]), frames.read(frame_bytes), "raw", "BGRX")

                    def pixels(frame):
                        return getattr(frame, "get_flattened_data", frame.getdata)()

                    until(lambda: count() == 1, "initial workspace frame missing")
                    scale = ready["width"] / output["width"]
                    red = (round((target["x"] - output["x"] + target["width"] / 2) * scale),
                           round((target["y"] - output["y"] + target["height"] / 4) * scale))
                    self.assertEqual(image().getpixel(red), (255, 0, 0))
                    self.assertNotIn((0, 255, 0), pixels(image()), "private workspace leaked")
                    time.sleep(.5)
                    idle = idle_cpu([compositor, stream], seconds=1.0)
                    print(json.dumps({"transport": transport, "size": [ready["width"], ready["height"]],
                                      "baseline_compositor_cpu_percent": round(baseline[0], 2),
                                      "sharing_compositor_cpu_percent": round(idle[0][0], 2),
                                      "sharing_portal_cpu_percent": round(idle[1][0], 2),
                                      "compositor_rss_delta_mib": round((idle[0][1] - baseline[1]) / 1024**2, 2),
                                      "portal_rss_mib": round(idle[1][1] / 1024**2, 2)}), flush=True)
                    # Unrelated commits must not compose/copy a new source frame.
                    for _ in range(8):
                        change(private_client, b"d")
                        time.sleep(.04)
                    self.assertEqual(count(), 1, "another workspace woke this capture")
                    # A callback-only commit starts an animation on a hidden workspace.
                    change(target_client, b"a")
                    marker = (round((target["x"] - output["x"] + target["width"] / 2 + 8) * scale),
                              round((target["y"] - output["y"] + target["height"] / 2 + 8) * scale))
                    # Capture coalesces intermediate commits under backpressure.
                    # Require the final animation state, not one frame per commit.
                    until(lambda: count() >= 2 and image().getpixel(marker) == (255, 255, 3),
                          lambda: f"hidden animation stalled at {count()} frames, pixel {image().getpixel(marker)}")
                    animation_count = count()
                    time.sleep(.2)
                    self.assertEqual(count(), animation_count, "static workspace emitted duplicates")
                    # Independent consumers share the composed view and both update.
                    other_stream, other_ready = worker()
                    second_raw = root / "second.bgrx"
                    other_consumer = consumer(second_raw, other_ready["node"])
                    until(lambda: count(second_raw) == 1, "second consumer did not receive cached frame")
                    self.assertEqual(image().tobytes(), image(second_raw).tobytes())
                    for index in range(6):
                        change(target_client, b"d")
                        until(lambda: count() >= animation_count + 1 + index and count(second_raw) >= 2 + index,
                              "one consumer missed source damage")
                    # A changed privacy rule must clear retained pixels in both views.
                    previous = count()
                    config.write_text(base_config + 'window-rule title="Capture target" block-out-from-screencasts=#true\n')
                    call("reload-config")
                    until(lambda: count() > previous, "privacy change did not invalidate retained image")
                    self.assertNotIn((255, 0, 0), pixels(image()))
                    self.assertNotIn((0, 0, 255), pixels(image()))
                    previous = count()
                    config.write_text(base_config)
                    call("reload-config")
                    until(lambda: count() > previous, "privacy opt-in did not repaint")
                    self.assertEqual(image().getpixel(red), (255, 0, 0))
                    # Pause long enough to release targets, then resume unchanged content.
                    for child in [first_consumer, other_consumer]:
                        os.killpg(child.pid, signal.SIGTERM)
                        child.wait(timeout=5)
                    time.sleep(2.3)
                    self.assertIn("released idle workspace capture target", log_path.read_text())
                    resumed = root / "resumed.bgrx"
                    resumed_consumer = consumer(resumed, ready["node"], frames=1)
                    until(lambda: count(resumed) == 1, "resumed consumer did not get an initial frame")
                    resumed_consumer.wait(timeout=5)
                    self.assertEqual(resumed_consumer.returncode, 0)
                    self.assertEqual(image(resumed).getpixel(red), (255, 0, 0))
                    self.assertEqual(next(ws["id"] for ws in call("workspaces") if ws["focused"]), private_workspace)
                    # Closing the source's last window removes that inactive workspace.
                    os.killpg(target_client.pid, signal.SIGTERM)
                    target_client.wait(timeout=5)
                    self.assertEqual(stream.wait(timeout=7), 0)
                    self.assertEqual(other_stream.wait(timeout=7), 0)
                    self.assertIsNone(compositor.poll(), log_path.read_text())
                finally:
                    for child in reversed(children):
                        if child.poll() is None:
                            os.killpg(child.pid, signal.SIGTERM)
                            try:
                                child.wait(timeout=5)
                            except subprocess.TimeoutExpired:
                                os.killpg(child.pid, signal.SIGKILL)
                                child.wait()
                        for pipe in [child.stdin, child.stdout]:
                            if pipe is not None and pipe is not log:
                                pipe.close()


if __name__ == "__main__":
    unittest.main()
