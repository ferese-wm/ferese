"""Open, update and close blurred popups on a private nested compositor.

FERESE_TEST_BLURRED_POPUP=1 FERESE_TEST_BINARY=target/release/ferese \\
    FERESE_TEST_CTL=target/release/feresectl python3 scripts/tests/test_blurred_popup_isolated.py
Requires a Wayland host, wayland-scanner and a C compiler.
"""
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest


@unittest.skipUnless(os.environ.get("FERESE_TEST_BLURRED_POPUP") == "1", "opt-in nested blur test")
class BlurredPopupTest(unittest.TestCase):
    def test_popup_lifecycle_keeps_presenting_frames(self):
        repo = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="ferese-blurred-popup-") as temporary:
            root = Path(temporary)
            protocols = Path(subprocess.check_output(
                ["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True
            ).strip())
            generated = []
            for name, xml in [
                ("xdg-shell", protocols / "stable/xdg-shell/xdg-shell.xml"),
                ("ferese-effects", repo / "protocols/ferese-effects-v1.xml"),
            ]:
                source = root / f"{name}.c"
                subprocess.run(["wayland-scanner", "client-header", str(xml),
                                str(root / f"{name}-client-protocol.h")], check=True)
                subprocess.run(["wayland-scanner", "private-code", str(xml), str(source)], check=True)
                generated.append(str(source))
            flags = subprocess.check_output(
                ["pkg-config", "--cflags", "--libs", "wayland-client"], text=True
            ).split()
            client = root / "client"
            subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-I", str(root),
                            str(repo / "scripts/tests/fixtures/blurred-popup.c"),
                            *generated, "-o", str(client), *flags], check=True)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            (config / "config.kdl").write_text('theme { material { style "translucent"; } }\n')
            host = Path(os.environ["WAYLAND_DISPLAY"])
            if not host.is_absolute():
                host = Path(os.environ["XDG_RUNTIME_DIR"]) / host
            env = dict(os.environ, WAYLAND_DISPLAY=str(host), XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       XDG_CACHE_HOME=str(root / "cache"), RUST_LOG="ferese=info")
            env.pop("WAYLAND_SOCKET", None)
            env.pop("FERESE_SHELL_CONTROL_SOCKET", None)
            binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/debug/ferese")
            log_path = root / "compositor.log"
            with log_path.open("w") as log:
                compositor = subprocess.Popen(
                    [str(binary), "--backend", "nested", "--grant-effects", "--", str(client)],
                    env=env, stdout=log, stderr=log,
                )
                try:
                    deadline = time.monotonic() + 30
                    while log_path.read_text().splitlines().count("close") < 3:
                        self.assertIsNone(compositor.poll(), log_path.read_text())
                        if time.monotonic() >= deadline:
                            self.fail("Blurred popup stopped presenting frames:\n" + log_path.read_text())
                        time.sleep(.025)
                    control = repo / os.environ.get("FERESE_TEST_CTL", "target/debug/feresectl")
                    subprocess.run([str(control), "-j", "outputs"], env=env, check=True,
                                   stdout=subprocess.DEVNULL, timeout=5)
                    phases = log_path.read_text().splitlines()
                    for name, count in [("parent", 1), ("open", 3), ("update", 9), ("close", 3)]:
                        self.assertEqual(phases.count(name), count, log_path.read_text())
                finally:
                    if compositor.poll() is None:
                        compositor.terminate()
                        try:
                            compositor.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            compositor.kill()
                            compositor.wait()


if __name__ == "__main__":
    unittest.main()
