"""Verify visibility-gated callbacks and idle inhibition on a private desktop.

FERESE_TEST_PRESENTATION=1 python3 scripts/tests/test_presentation_isolated.py
Requires built debug compositor/control binaries, a Wayland host, cc and wayland-scanner.
"""
import json
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import signal
import subprocess
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[2]


@unittest.skipUnless(os.environ.get("FERESE_TEST_PRESENTATION") == "1", "requires a Wayland host")
class Presentation(unittest.TestCase):
    def test_covered_and_unmapped_inhibitors_stop_inhibiting(self):
        processes = []
        with tempfile.TemporaryDirectory(prefix="ferese-presentation-") as directory:
            root = Path(directory)
            protocols = subprocess.check_output(["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True).strip()
            sources = []
            for name, relative in [("xdg-shell", "stable/xdg-shell/xdg-shell.xml"),
                                   ("idle-inhibit", "unstable/idle-inhibit/idle-inhibit-unstable-v1.xml"),
                                   ("idle-notify", "staging/ext-idle-notify/ext-idle-notify-v1.xml")]:
                xml = str(Path(protocols) / relative)
                for kind, suffix in [("client-header", "client-protocol.h"), ("private-code", "protocol.c")]:
                    subprocess.run(["wayland-scanner", kind, xml, str(root / f"{name}-{suffix}")], check=True)
                sources.append(str(root / f"{name}-protocol.c"))
            flags = subprocess.check_output(["pkg-config", "--cflags", "--libs", "wayland-client"], text=True).split()
            subprocess.run(["cc", "-Wall", "-Wextra", "-I", str(root), str(REPO / "scripts/tests/fixtures/presentation.c"),
                            *sources, "-o", str(root / "client"), *flags], check=True)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese/config.kdl"
            config.parent.mkdir(parents=True)
            config.write_text('window-rule app-id="ferese.test.presentation" floating=#true\n'
                              'animations { enabled #false; }\n'
                              'theme { geometry { window-radius 0; border-width 0; focus-ring-width 0; }; }\n')
            # Remembered geometry deliberately places both clients together;
            # ordinary placement now avoids overlap. Keep host state isolated.
            state = root / "state/ferese"
            state.mkdir(parents=True)
            (state / "floating.json").write_text(json.dumps({
                "ferese.test.presentation": {
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
                compositor = subprocess.Popen([str(REPO / "target/debug/ferese"), "--backend=nested"], env=ipc_environment(env), stdout=log, stderr=log, start_new_session=True)
                processes.append(compositor)
                deadline = time.monotonic() + 10
                while not (ipc_socket(runtime)).exists():
                    if compositor.poll() is not None or time.monotonic() > deadline:
                        self.fail((root / "compositor.log").read_text())
                    time.sleep(0.02)
                sockets = list(runtime.glob("wayland-*"))
                env["WAYLAND_DISPLAY"] = str(next(path for path in sockets if not path.name.endswith(".lock")))

                def call(*args):
                    return subprocess.check_output([str(REPO / "target/debug/feresectl"), "-j", *args], env=ipc_environment(env), timeout=10)

                def windows(count):
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        result = json.loads(call("get-windows"))
                        if len(result) == count and all(window["mapped"] and window["width"] == 640 for window in result):
                            return result
                        time.sleep(0.02)
                    self.fail((root / "compositor.log").read_text())

                events = (root / "events").open("w")
                target = subprocess.Popen([str(root / "client")], env=ipc_environment(env), stdin=subprocess.PIPE,
                                          stdout=events, stderr=log, start_new_session=True)
                processes.append(target)
                windows(1)
                time.sleep(.4)

                def count(event):
                    return (root / "events").read_text().splitlines().count(event)

                before = count("frame")
                idle = count("idle")
                time.sleep(.5)
                self.assertGreater(count("frame"), before, "visible empty commits must get callbacks")
                self.assertEqual(count("idle"), idle, "visible inhibitor must keep the notifier active")

                cover = subprocess.Popen([str(root / "client"), "cover"], env=ipc_environment(env), stdout=log,
                                         stderr=log, start_new_session=True)
                processes.append(cover)
                current = windows(2)
                self.assertEqual((current[0]["x"], current[0]["y"]), (current[1]["x"], current[1]["y"]))
                time.sleep(.5)
                before = count("frame")
                time.sleep(.5)
                self.assertEqual(count("frame"), before, "fully occluded client must stop receiving callbacks")
                self.assertGreater(count("idle"), idle, "occluded inhibitor must stop inhibiting")

                cover.terminate()
                cover.wait(timeout=5)
                windows(1)
                time.sleep(.5)
                self.assertGreater(count("frame"), before, "revealed client must resume callbacks")
                idle = count("idle")
                time.sleep(.4)
                self.assertEqual(count("idle"), idle)
                target.stdin.write(b"u")
                target.stdin.flush()
                time.sleep(.6)
                self.assertGreater(count("idle"), idle, "unmapped inhibitor must stop inhibiting")
                target.stdin.close()
                events.close()
            finally:
                for process in reversed(processes):
                    if process.poll() is None:
                        os.killpg(process.pid, signal.SIGTERM)
                        process.wait(timeout=5)
                log.close()


if __name__ == "__main__":
    unittest.main()
