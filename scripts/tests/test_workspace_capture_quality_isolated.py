"""Compare workspace capture detail through the real portal and PipeWire.

FERESE_TEST_WORKSPACE_CAPTURE_QUALITY=1 python3 scripts/tests/test_workspace_capture_quality_isolated.py
Requires a Wayland host, PipeWire, wayland-scanner, cc, and Pillow. The private
nested output must exceed the 1080p capture bounds to exercise downsampling.
Only a synthetic striped window is shared; the host desktop is never captured.
"""
import io
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


@unittest.skipUnless(os.environ.get("FERESE_TEST_WORKSPACE_CAPTURE_QUALITY") == "1",
                     "requires Wayland and PipeWire")
class WorkspaceCaptureQuality(unittest.TestCase):
    def test_native_default_preserves_detail_and_quality_views_stay_independent(self):
        from PIL import Image
        children = []
        with tempfile.TemporaryDirectory(prefix="ferese-workspace-quality-") as temporary:
            root = Path(temporary)
            protocols = Path(subprocess.check_output(
                ["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True).strip())
            protocol_sources = []
            for protocol, relative in [("xdg-shell", "stable/xdg-shell/xdg-shell.xml"),
                                       ("viewporter", "stable/viewporter/viewporter.xml")]:
                source = root / f"{protocol}-protocol.c"
                subprocess.run(["wayland-scanner", "client-header", str(protocols / relative),
                                str(root / f"{protocol}-client-protocol.h")], check=True)
                subprocess.run(["wayland-scanner", "private-code", str(protocols / relative),
                                str(source)], check=True)
                protocol_sources.append(str(source))
            for fixture, name, package, extra in [
                ("window-capture-scale.c", "client", "wayland-client", protocol_sources),
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
            wallpaper_color = (17, 39, 73)
            wallpaper = root / "wallpaper.png"
            Image.new("RGB", (1, 1), wallpaper_color).save(wallpaper)
            config.write_text('animations { reduced-motion #true; }\n'
                              'theme { mode "dark"; background { path ' + json.dumps(str(wallpaper)) + '; }; }\n'
                              'appearance { window-effects { enabled #false; blur #false; }; }\n'
                              'window-rule app-id="ferese.test.capture-scale" floating=#true width=640.0 height=480.0\n')
            subprocess.run([str(COMPOSITOR), "--check-config", str(config)], check=True)
            host_runtime = Path(os.environ["XDG_RUNTIME_DIR"])
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = host_runtime / display
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=str(display),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       XDG_CACHE_HOME=str(root / "cache"), XDG_DATA_HOME=str(root / "data"),
                       FERESE_ENABLE_SCREENCOPY="1", FERESE_CAPTURE_FORCE_SHM="1",
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

                def until(predicate, message, timeout=10):
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        result = predicate()
                        if result:
                            return result
                        if compositor.poll() is not None:
                            self.fail(log_path.read_text())
                        time.sleep(.02)
                    self.fail(message + "\n" + log_path.read_text())

                def call(*args):
                    return json.loads(subprocess.check_output(
                        [str(CTL), "-j", *args], env=ipc_environment(env), timeout=5))

                def close_worker(worker):
                    worker.stdin.close()
                    worker.wait(timeout=7)
                    self.assertEqual(worker.returncode, 0, log_path.read_text())
                    worker.stdout.close()

                try:
                    compositor = launch([str(COMPOSITOR), "--backend=nested"])
                    until(lambda: ipc_socket(runtime).is_socket(), "nested compositor did not start")
                    env["WAYLAND_DISPLAY"] = str(until(
                        lambda: next((p for p in runtime.glob("wayland-*") if p.is_socket()), None),
                        "nested Wayland socket missing"))
                    # The host can replace Winit's requested initial size with
                    # its tiled allocation shortly after the socket appears.
                    # Measure the settled private output, not that startup size.
                    last_output = None
                    settled_since = time.monotonic()

                    def settled_output():
                        nonlocal last_output, settled_since
                        current = call("outputs")[0]
                        identity = (current["current_mode"], current["scale"], current["transform"])
                        if identity != last_output:
                            last_output = identity
                            settled_since = time.monotonic()
                        return current if time.monotonic() - settled_since >= .3 else None

                    output = until(settled_output, "nested output did not settle")
                    mode = output["current_mode"]
                    native = (mode["width"], mode["height"])
                    if output["transform"] in ("90", "270", "flipped-90", "flipped-270"):
                        native = native[::-1]
                    long, short = sorted(native, reverse=True)
                    reduction = min(1920 / long, 1080 / short, 1)
                    if reduction > .8:
                        self.skipTest(f"private output {native} does not sufficiently exceed 1080p bounds")
                    # Native resolution still obeys aggregate capture admission.
                    # Pair Native with Balanced, then release both before High.
                    aligned_row = (native[0] * 4 + 4095) & ~4095
                    if aligned_row * native[1] * 10 > 400 * 1024**2:
                        self.skipTest(f"private output {native} exceeds the paired-stream test budget")
                    # Readiness is observed through the real capture path;
                    # asynchronous wallpaper decoding must finish before the
                    # quality-view equality workload starts.
                    def wallpaper_ready():
                        encoded = subprocess.check_output([str(CTL), "screenshot"],
                                                          env=ipc_environment(env), timeout=5)
                        image = Image.open(io.BytesIO(encoded)).convert("RGB")
                        return image.getextrema() == tuple((value, value) for value in wallpaper_color)

                    until(wallpaper_ready, "fixture wallpaper did not reach capture")
                    env["FERESE_TEST_OUTPUT_SCALE"] = str(output["scale"])
                    client = launch([str(root / "client"), "workspace-native"], stdin=subprocess.PIPE)
                    window = until(lambda: next((w for w in call("windows")
                                                  if w["title"] == "workspace-native" and w["mapped"]), None),
                                   "striped window did not map")
                    workspace = window["workspace"]
                    active = next(w["id"] for w in call("workspaces") if w["focused"])

                    def expected(quality):
                        if quality is None or quality == "native":
                            return native
                        cap = (1920, 1080) if quality == "balanced" else (2560, 1440)
                        if native[0] < native[1]:
                            cap = cap[::-1]
                        factor = min(cap[0] / native[0], cap[1] / native[1], 1)
                        return tuple(int(dimension * factor + .5) for dimension in native)

                    def worker(quality):
                        command = [str(PORTAL), "--stream-workspace", str(workspace), "hidden"]
                        if quality is not None:
                            command += ["--quality", quality]
                        stream = launch(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
                        with selectors.DefaultSelector() as selector:
                            selector.register(stream.stdout, selectors.EVENT_READ)
                            self.assertTrue(selector.select(10), log_path.read_text())
                        line = stream.stdout.readline()
                        self.assertTrue(line, log_path.read_text())
                        ready = json.loads(line)
                        size = (ready["width"], ready["height"])
                        self.assertEqual(size, expected(quality),
                                         "capture quality did not reach the compositor and PipeWire")
                        self.assertEqual(ready["logical_size"], [output["width"], output["height"]])
                        raw = root / f"{quality or 'default'}.bgrx"
                        consumer = launch([str(root / "consumer"), str(ready["node"]), "shm", "2", str(raw)])
                        frame_bytes = size[0] * size[1] * 4
                        until(lambda: raw.exists() and raw.stat().st_size >= frame_bytes,
                              f"{quality or 'default'} initial frame missing")
                        return stream, consumer, raw, size

                    def frame(raw, size):
                        frame_bytes = size[0] * size[1] * 4
                        return Image.frombytes("RGB", size, raw.read_bytes()[:frame_bytes], "raw", "BGRX")

                    native_stream, native_consumer, native_raw, native_size = worker(None)
                    balanced_stream, balanced_consumer, balanced_raw, balanced_size = worker("balanced")
                    native_image = frame(native_raw, native_size)
                    balanced_image = frame(balanced_raw, balanced_size)
                    client.stdin.write(b"d")
                    client.stdin.flush()
                    for consumer in (native_consumer, balanced_consumer):
                        consumer.wait(timeout=7)
                        self.assertEqual(consumer.returncode, 0, log_path.read_text())
                    for raw, size in ((native_raw, native_size), (balanced_raw, balanced_size)):
                        frame_bytes = size[0] * size[1] * 4
                        contents = raw.read_bytes()
                        self.assertEqual(len(contents), frame_bytes * 2)
                        self.assertEqual(contents[:frame_bytes], contents[frame_bytes:],
                                         "one quality view corrupted another view's retained image")
                    close_worker(native_stream)
                    close_worker(balanced_stream)
                    high_stream, high_consumer, high_raw, high_size = worker("high")
                    high_image = frame(high_raw, high_size)
                    client.stdin.write(b"d")
                    client.stdin.flush()
                    high_consumer.wait(timeout=7)
                    self.assertEqual(high_consumer.returncode, 0, log_path.read_text())
                    close_worker(high_stream)

                    # Compare the same physical-pixel region after OBS-like
                    # enlargement. The client itself renders at the output's
                    # backing density; this isolates the extra capture downsample.
                    scale = output["scale"]
                    left = max(window["x"] - output["x"] + 32, 32)
                    right = min(window["x"] - output["x"] + window["width"] - 32,
                                output["width"] - 32)
                    y = window["y"] - output["y"] + window["height"] / 2
                    self.assertGreater(right - left, 128, "pattern is clipped by the private output")
                    x0, x1 = int(left * scale), int(right * scale)
                    y0 = int(y * scale)

                    def edge_contrast(image):
                        sample = image.convert("L").crop((x0, y0, x1, y0 + 1))
                        row = list(getattr(sample, "get_flattened_data", sample.getdata)())
                        return sum(abs(a - b) for a, b in zip(row, row[1:])) / (len(row) - 1)

                    artifact_dir = os.environ.get("FERESE_TEST_CAPTURE_QUALITY_ARTIFACTS")
                    if artifact_dir:
                        destination = Path(artifact_dir)
                        destination.mkdir(parents=True, exist_ok=True)
                        native_image.save(destination / "native.png")
                        balanced_image.save(destination / "balanced.png")
                        high_image.save(destination / "high.png")
                        (destination / "capture.json").write_text(json.dumps({
                            "output": output, "initial_window": window, "windows": call("windows"),
                            "sample_region": [x0, y0, x1, y0 + 1], "config": config.read_text(),
                        }, indent=2))
                    detail = edge_contrast(native_image)
                    balanced_detail = edge_contrast(balanced_image.resize(native, Image.Resampling.BILINEAR))
                    high_detail = edge_contrast(high_image.resize(native, Image.Resampling.BILINEAR))
                    self.assertGreater(detail, 100, "native capture unexpectedly filtered the one-pixel pattern")
                    self.assertGreater(detail, balanced_detail * 1.35,
                                       "native capture did not retain more detail than enlarged 1080p")
                    self.assertEqual(next(w["id"] for w in call("workspaces") if w["focused"]), active)
                    print(json.dumps({"output_scale": scale, "native": native_size,
                                      "balanced": balanced_size, "high": high_size,
                                      "native_edge_contrast": round(detail, 2),
                                      "enlarged_balanced_edge_contrast": round(balanced_detail, 2),
                                      "enlarged_high_edge_contrast": round(high_detail, 2)}), flush=True)
                finally:
                    for child in reversed(children):
                        if child.poll() is None:
                            os.killpg(child.pid, signal.SIGTERM)
                            try:
                                child.wait(timeout=5)
                            except subprocess.TimeoutExpired:
                                os.killpg(child.pid, signal.SIGKILL)
                                child.wait()
                        for pipe in (child.stdin, child.stdout):
                            if pipe is not None and pipe is not log:
                                pipe.close()


if __name__ == "__main__":
    unittest.main()
