"""Controlled ack/commit resize test on a disposable nested desktop.

FERESE_TEST_RESIZE=1 FERESE_TEST_BINARY=target/release/ferese \
FERESE_TEST_CTL=target/release/feresectl python3 scripts/tests/test_resize_dependencies_isolated.py
Requires a Wayland host, cc, pkg-config and wayland-scanner. This checks state
before the 300 ms deadline; it does not measure frame pacing or GPU performance.
"""
import json
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import select
import subprocess
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[2]


@unittest.skipUnless(os.environ.get("FERESE_TEST_RESIZE") == "1", "opt-in nested test")
class ResizeDependencies(unittest.TestCase):
    def test_independent_translation_before_resized_buffer(self):
        processes = []
        with tempfile.TemporaryDirectory(prefix="ferese-resize-") as directory:
            root = Path(directory)
            protocols = Path(subprocess.check_output(
                ["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True).strip())
            xml = str(protocols / "stable/xdg-shell/xdg-shell.xml")
            for kind, name in [("client-header", "xdg-shell-client-protocol.h"),
                               ("private-code", "xdg-shell-protocol.c")]:
                subprocess.run(["wayland-scanner", kind, xml, str(root / name)], check=True)
            flags = subprocess.check_output(["pkg-config", "--cflags", "--libs", "wayland-client"], text=True).split()
            subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-I", str(root),
                            str(REPO / "scripts/tests/fixtures/slow-resize.c"),
                            str(root / "xdg-shell-protocol.c"), "-o", str(root / "client"), *flags], check=True)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese/config.kdl"
            config.parent.mkdir(parents=True)
            config.write_text('layout { inner-gap 0; outer-gap 0; }\n'
                              'theme { geometry { window-radius 0; border-width 0; focus-ring-width 0; }; }\n')
            host = Path(os.environ["WAYLAND_DISPLAY"])
            if not host.is_absolute():
                host = Path(os.environ["XDG_RUNTIME_DIR"]) / host
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(root / "config"),
                       XDG_STATE_HOME=str(root / "state"), WAYLAND_DISPLAY=str(host))
            for key in ["WAYLAND_SOCKET", "FERESE_SOCKET", "FERESE_SHELL_CONTROL_SOCKET"]:
                env.pop(key, None)
            binary = REPO / os.environ.get("FERESE_TEST_BINARY", "target/debug/ferese")
            ctl = REPO / os.environ.get("FERESE_TEST_CTL", "target/debug/feresectl")
            with (root / "compositor.log").open("w") as log:
                try:
                    compositor = subprocess.Popen([str(binary), "--backend=nested"], env=ipc_environment(env), stdout=log, stderr=log)
                    processes.append(compositor)
                    deadline = time.monotonic() + 10
                    while not (ipc_socket(runtime)).exists():
                        if compositor.poll() is not None or time.monotonic() > deadline:
                            self.fail((root / "compositor.log").read_text())
                        time.sleep(.02)
                    socket = next(p for p in runtime.glob("wayland-*") if not p.name.endswith(".lock"))
                    client_env = dict(env, WAYLAND_DISPLAY=str(socket))

                    def call(*args):
                        return json.loads(subprocess.check_output([str(ctl), "-j", *args], env=ipc_environment(env), timeout=5))

                    def action(*args):
                        subprocess.run([str(ctl), *args], env=ipc_environment(env), check=True, capture_output=True, timeout=5)

                    def client(name):
                        process = subprocess.Popen([str(root / "client"), name], env=client_env,
                                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                                   stderr=log, bufsize=0)
                        processes.append(process)
                        return process

                    pending = {}

                    def marker(process, kind, timeout=5):
                        deadline = time.monotonic() + timeout
                        data = pending.pop(process.pid, b"")
                        while True:
                            while b"\n" in data:
                                line, data = data.split(b"\n", 1)
                                fields = line.decode().split()
                                if fields[0] == kind:
                                    pending[process.pid] = data
                                    return list(map(int, fields[1:]))
                            remaining = deadline - time.monotonic()
                            self.assertGreater(remaining, 0, f"missing {kind} marker")
                            ready, _, _ = select.select([process.stdout], [], [], remaining)
                            self.assertTrue(ready, f"missing {kind} marker")
                            chunk = os.read(process.stdout.fileno(), 4096)
                            self.assertTrue(chunk, (root / "compositor.log").read_text())
                            data += chunk

                    def windows(count):
                        deadline = time.monotonic() + 5
                        while time.monotonic() < deadline:
                            current = call("get-windows")
                            if len(current) == count and all(w["mapped"] for w in current):
                                return {w["app_id"]: w for w in current}
                            time.sleep(.02)
                        self.fail((root / "compositor.log").read_text())

                    first = client("ferese.test.first")
                    marker(first, "committed")
                    windows(1)
                    slow = client("ferese.test.slow")
                    marker(slow, "committed")
                    windows(2)
                    # Settle opening/focus motion before isolating one configure.
                    time.sleep(1)
                    slow.stdin.write(b"h")
                    marker(slow, "holding")
                    before = windows(2)
                    original = before["ferese.test.slow"]
                    started = time.monotonic()
                    action("resize", "right")
                    configured = marker(slow, "held", timeout=.25)
                    self.assertGreater(configured[1], original["capture_width"])
                    # The ack has reached the compositor; no new buffer exists.
                    action("focus", "left")
                    action("center-column")
                    moved = None
                    while time.monotonic() - started < .25:
                        current = windows(2)
                        observed_elapsed = time.monotonic() - started
                        if observed_elapsed >= .25:
                            break
                        a, b = current["ferese.test.first"], current["ferese.test.slow"]
                        if abs(a["x"] - before["ferese.test.first"]["x"]) >= 2:
                            moved = current
                            break
                        time.sleep(.005)
                    self.assertIsNotNone(moved, "viewport did not move before the resize deadline")
                    a, b = moved["ferese.test.first"], moved["ferese.test.slow"]
                    self.assertEqual(b["capture_width"], original["capture_width"])
                    self.assertEqual(b["width"], original["width"])
                    self.assertAlmostEqual(a["x"] - before["ferese.test.first"]["x"],
                                           b["x"] - original["x"], delta=1)
                    slow.stdin.write(b"c")
                    committed = marker(slow, "committed")
                    self.assertEqual(committed[1:], configured[1:])
                    self.assertEqual(windows(2)["ferese.test.slow"]["capture_width"], configured[1])
                    print(f"nested: movement observed at {observed_elapsed * 1000:.1f} ms; "
                          "held raster/size; resized SHM commit (state check, not frame pacing)")
                finally:
                    for process in reversed(processes):
                        if process.poll() is None:
                            process.terminate()
                            process.wait(timeout=5)
                        for stream in [process.stdin, process.stdout]:
                            if stream is not None:
                                stream.close()


if __name__ == "__main__":
    unittest.main()
