"""Opt-in XDG buffer lifecycle test for a disposable nested compositor.

FERESE_TEST_UNMAPPED=1 python3 scripts/tests/test_unmapped_toplevel_isolated.py
Requires cc, pkg-config, wayland-scanner, and wayland-protocols.
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


@unittest.skipUnless(os.environ.get("FERESE_TEST_UNMAPPED") == "1", "opt-in nested test")
class UnmappedToplevelTest(unittest.TestCase):
    def test_only_buffered_toplevels_take_layout_space(self):
        repo = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="ferese-unmapped-test-") as temporary:
            root = Path(temporary)
            protocols = subprocess.check_output(
                ["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True
            ).strip()
            xml = str(Path(protocols) / "stable/xdg-shell/xdg-shell.xml")
            for kind, output in [
                ("client-header", "xdg-shell-client-protocol.h"),
                ("private-code", "xdg-shell-protocol.c"),
            ]:
                subprocess.run(["wayland-scanner", kind, xml, str(root / output)], check=True)
            flags = subprocess.check_output(
                ["pkg-config", "--cflags", "--libs", "wayland-client"], text=True
            ).split()
            subprocess.run(
                [
                    "cc", "-Wall", "-Wextra", "-I", str(root),
                    str(repo / "scripts/tests/fixtures/unmapped-toplevel.c"),
                    str(root / "xdg-shell-protocol.c"), "-o", str(root / "client"),
                    *flags,
                ],
                check=True,
            )

            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            (config / "config.kdl").write_text("\n")
            host_display = os.environ["WAYLAND_DISPLAY"]
            if not host_display.startswith("/"):
                host_display = str(Path(os.environ["XDG_RUNTIME_DIR"]) / host_display)
            env = dict(
                os.environ,
                XDG_RUNTIME_DIR=str(runtime),
                XDG_CONFIG_HOME=str(root / "config"),
                WAYLAND_DISPLAY=host_display,
                RUST_LOG="ferese=info,ferese::state::lifecycle=debug,ferese::state::animation=debug",
            )
            compositor_binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/debug/ferese")
            ctl_binary = repo / os.environ.get("FERESE_TEST_CTL", "target/debug/feresectl")
            with (root / "compositor.log").open("w") as log:
                compositor = subprocess.Popen(
                    [str(compositor_binary), "--backend", "nested"],
                    env=ipc_environment(env),
                    stdout=log,
                    stderr=subprocess.STDOUT,
                )
                client = None
                try:
                    for _ in range(100):
                        if compositor.poll() is not None:
                            self.fail((root / "compositor.log").read_text())
                        sockets = [
                            path for path in runtime.glob("wayland-*")
                            if not path.name.endswith(".lock")
                        ]
                        if sockets:
                            break
                        time.sleep(0.1)
                    self.assertTrue(sockets, "nested Wayland socket did not appear")
                    client = subprocess.Popen(
                        [str(root / "client")],
                        env=dict(env, WAYLAND_DISPLAY=str(sockets[0])),
                        stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE,
                        text=True,
                    )

                    visible_ids = []
                    for phase, visible in [
                        ("initial-empty", False),
                        ("visible", True),
                        ("detached", False),
                        ("remapped", True),
                    ]:
                        ready, _, _ = select.select([client.stdout], [], [], 10)
                        self.assertTrue(ready, f"client did not reach {phase}")
                        self.assertEqual(client.stdout.readline().strip(), phase)
                        deadline = time.monotonic() + 3
                        while True:
                            result = subprocess.run(
                                [str(ctl_binary), "-j", "focused-window"],
                                env=ipc_environment(env),
                                capture_output=True,
                                text=True,
                                check=True,
                            )
                            focused = json.loads(result.stdout)
                            if (focused is not None and focused.get("id") is not None) == visible:
                                break
                            if time.monotonic() >= deadline:
                                self.fail(f"{phase}: unexpected focused window {focused}")
                            time.sleep(0.05)
                        if visible:
                            visible_ids.append(focused["id"])
                        if phase == "visible":
                            # Leave a configure/resize outstanding when the buffer is detached.
                            subprocess.run([str(ctl_binary), "toggle-maximized"], env=ipc_environment(env),
                                           capture_output=True, check=True)
                            time.sleep(.08)
                        if phase == "detached":
                            self.assertIn("retained close presentation", (root / "compositor.log").read_text(),
                                          "unmap lost the old buffer before snapshot capture")

                        client.stdin.write("\n")
                        client.stdin.flush()
                    self.assertNotEqual(visible_ids[0], visible_ids[1], "remapping must allocate a fresh window identity")
                    self.assertEqual(client.wait(timeout=5), 0)

                    deadline = time.monotonic() + 3
                    while True:
                        remaining = json.loads(subprocess.check_output(
                            [str(ctl_binary), "-j", "windows"], env=ipc_environment(env), text=True
                        ))
                        if not remaining:
                            break
                        if time.monotonic() >= deadline:
                            self.fail(f"destroyed window still managed: {remaining}")
                        time.sleep(0.05)

                    subprocess.run([str(ctl_binary), "focus-last-window"], env=ipc_environment(env),
                                   capture_output=True, check=True)
                    focused = json.loads(subprocess.check_output(
                        [str(ctl_binary), "-j", "focused-window"], env=ipc_environment(env), text=True
                    ))
                    self.assertIsNone(focused, "focus history must not restore a destroyed window")
                    deadline = time.monotonic() + 3
                    while "released close presentation" not in (root / "compositor.log").read_text():
                        if time.monotonic() >= deadline:
                            self.fail("retained close texture did not settle and release")
                        time.sleep(.025)
                finally:
                    if client and client.poll() is None:
                        client.terminate()
                        client.wait(timeout=5)
                    if client:
                        client.stdin.close()
                        client.stdout.close()
                    compositor.terminate()
                    compositor.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
