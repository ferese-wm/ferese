"""Check invalid-config startup, its notification and recovery on a private desktop.

FERESE_TEST_STARTUP_CONFIG=1 python3 scripts/tests/test_startup_config_isolated.py
Uses release binaries by default. Requires a Wayland host, dbus-daemon and gdbus.
"""
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time
import unittest

from session_socket import ipc_environment, ipc_socket


@unittest.skipUnless(os.environ.get("FERESE_TEST_STARTUP_CONFIG") == "1", "opt-in nested startup test")
class StartupConfigTest(unittest.TestCase):
    def test_defaults_notification_and_recovery_preserve_strict_validation(self):
        repo = Path(__file__).resolve().parents[2]
        binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/release/ferese")
        shell = repo / os.environ.get("FERESE_TEST_SHELL", "target/release/ferese-shell")
        ctl = repo / os.environ.get("FERESE_TEST_CTL", "target/release/feresectl")
        with tempfile.TemporaryDirectory(prefix="ferese-startup-config-") as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            directory = root / "config/ferese"
            directory.mkdir(parents=True)
            path = directory / "config.kdl"
            invalid = ('// keep this file\n'
                       'notifications { do-not-disturb #true; show-popups #false; }\n'
                       'panel main { surface solid; group-surface none; }\n')
            path.write_text(invalid)
            checked = subprocess.run([str(binary), "--check-config", str(path)],
                                     capture_output=True, text=True, timeout=15)
            self.assertNotEqual(checked.returncode, 0)
            self.assertRegex(checked.stderr, r"unknown field `(surface|group_surface)`")

            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
            env = dict(os.environ, WAYLAND_DISPLAY=str(display), XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       XDG_CACHE_HOME=str(root / "cache"), XDG_DATA_HOME=str(root / "data"))
            for name in ("WAYLAND_SOCKET", "FERESE_SHELL_CONTROL_SOCKET", "DISPLAY", "XAUTHORITY"):
                env.pop(name, None)
            # No activation directories: every notification stays on this private bus.
            bus_config = root / "bus.conf"
            bus_config.write_text(
                '<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
                '<policy context="default"><allow send_destination="*"/>'
                '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
            log_path = root / "session.log"
            children = []
            with log_path.open("w") as log:
                def launch(command, **options):
                    child = subprocess.Popen(command, env=ipc_environment(env), start_new_session=True,
                                             stderr=log, **options)
                    children.append(child)
                    return child

                def control(*args, check=True):
                    return subprocess.run([str(ctl), *args], env=ipc_environment(env), capture_output=True,
                                          text=True, timeout=5, check=check)

                def theme():
                    result = control("theme", "get", "--json", check=False)
                    return json.loads(result.stdout) if result.returncode == 0 else None

                def wait_for(predicate):
                    deadline = time.monotonic() + 30
                    while time.monotonic() < deadline:
                        value = predicate()
                        if value:
                            return value
                        if any(child.poll() is not None for child in children):
                            self.fail("A test process exited:\n" + log_path.read_text()[-10000:])
                        time.sleep(.05)
                    self.fail("Timed out:\n" + log_path.read_text()[-10000:])

                def notification_presented():
                    trace = log_path.read_text()
                    surface = re.search(
                        r'get_layer_surface\([^\n]*wl_surface[@#](\d+)[^\n]*"ferese-shell-notifications"', trace)
                    if not surface:
                        return False
                    identity = surface.group(1)
                    lifetime = trace[surface.start():]
                    attached = re.search(r'wl_surface[@#]' + identity + r'\.attach\(wl_buffer[@#]', lifetime)
                    return attached is not None and re.search(
                        r'wl_surface[@#]' + identity + r'\.commit\(', lifetime[attached.end():]) is not None

                try:
                    bus = launch(["dbus-daemon", "--config-file=" + str(bus_config), "--nofork", "--print-address=1"],
                                 stdout=subprocess.PIPE, text=True)
                    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                    bus.stdout.close()
                    launch([str(binary), "--backend", "nested", "--grant-effects", "--grant-shell-control",
                            "--", "env", "WAYLAND_DEBUG=client", "ICED_BACKEND=tiny-skia", str(shell)], stdout=log)
                    wait_for(lambda: ipc_socket(runtime).is_socket())
                    snapshot = wait_for(theme)
                    self.assertIn("default settings", snapshot["error"])
                    self.assertIn(str(path), snapshot["error"])
                    wait_for(lambda: '"ferese-shell-top-bar"' in log_path.read_text())
                    wait_for(lambda: "ferese-shell: configuration warning:" in log_path.read_text())
                    wait_for(notification_presented)
                    self.assertEqual(path.read_text(), invalid)

                    valid = '// corrected by the user\ntheme { accent "#83AA5F"; }\n'
                    path.write_text(valid)
                    control("reload-config")
                    accepted = wait_for(lambda: (value := theme()) and value["error"] is None and value)
                    self.assertEqual(accepted["theme"]["requested_accent"], "#83AA5F")
                    self.assertNotEqual(accepted["theme"]["tokens"]["colors"]["accent"],
                                        snapshot["theme"]["tokens"]["colors"]["accent"])
                    self.assertEqual(path.read_text(), valid)
                    self.assertEqual(log_path.read_text().count("ferese-shell: configuration warning:"), 1)

                    path.write_text(invalid)
                    rejected = control("reload-config", check=False)
                    self.assertNotEqual(rejected.returncode, 0)
                    retained = wait_for(lambda: (value := theme()) and value["error"] and value)
                    self.assertEqual(retained["theme"], accepted["theme"])
                    self.assertEqual(path.read_text(), invalid)
                finally:
                    for child in reversed(children):
                        if child.poll() is None:
                            os.killpg(child.pid, signal.SIGINT)
                            try:
                                child.wait(timeout=8)
                            except subprocess.TimeoutExpired:
                                os.killpg(child.pid, signal.SIGKILL)
                                child.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
