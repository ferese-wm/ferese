"""Opt-in theme integration test against a disposable nested desktop.

FERESE_TEST_THEME=1 python3 scripts/tests/test_theme_isolated.py
Requires target/debug/ferese, feresectl, and a running Wayland desktop.
"""
import json
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import select
import signal
import shutil
import subprocess
import tempfile
import time
import unittest


@unittest.skipUnless(os.environ.get("FERESE_TEST_THEME") == "1", "nested theme test is opt-in")
class ThemeTest(unittest.TestCase):
    def test_modes_files_preview_and_idle_subscription(self):
        repo = Path(__file__).resolve().parents[2]
        children = []
        with tempfile.TemporaryDirectory(prefix="ferese-theme-test-") as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            source = config / "config.kdl"
            shared = config / "themes/shared.kdl"
            shared.parent.mkdir()
            shared.write_text('colors { accent "#3D7BE6"; }\n')
            source.write_text('// Keep my comment\ntheme { mode "dark"; file "themes/shared.kdl"; material { style "translucent"; opacity 0.78; }; }\n')
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
            env = dict(os.environ, WAYLAND_DISPLAY=str(display), XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / "config"))

            def launch(command, **kwargs):
                child = subprocess.Popen(command, env=ipc_environment(env), start_new_session=True, **kwargs)
                children.append(child)
                return child

            def command(*args):
                return json.loads(subprocess.check_output([str(repo / "target/debug/feresectl"), "-j", *args], env=ipc_environment(env), timeout=5))

            def until(predicate, seconds=5):
                deadline = time.monotonic() + seconds
                while time.monotonic() < deadline:
                    try:
                        value = predicate()
                        if value:
                            return value
                    except (subprocess.SubprocessError, ConnectionError):
                        pass
                    time.sleep(.025)
                self.fail("Theme did not reach expected state")

            def replace(path, text):
                temporary = path.with_suffix(".new")
                temporary.write_text(text)
                temporary.replace(path)

            with (root / "compositor.log").open("w") as log:
                try:
                    bus = launch(["dbus-daemon", "--session", "--nofork", "--print-address=1"], stdout=subprocess.PIPE, text=True)
                    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                    compositor = launch([str(repo / "target/debug/ferese"), "--backend", "nested"], stdout=log, stderr=log)
                    until(lambda: (ipc_socket(runtime)).is_socket())
                    self.assertEqual(command("theme", "get")["theme"]["appearance"], "dark")
                    subscriber = launch([str(repo / "target/debug/feresectl"), "theme", "subscribe"], stdout=subprocess.PIPE, text=True)
                    self.assertTrue(select.select([subscriber.stdout], [], [], 3)[0])
                    initial = json.loads(subscriber.stdout.readline())
                    command("theme", "mode", "light")
                    ready = until(lambda: (snapshot := command("theme", "get"))["presented"] == snapshot["theme"] and snapshot["theme"]["appearance"] == "light" and snapshot)
                    self.assertEqual(ready["mode"], "light")
                    self.assertIn("// Keep my comment", source.read_text())
                    self.assertGreater(ready["revision"], initial["revision"])
                    candidate = root / "candidate.kdl"
                    candidate.write_text('theme { mode "dark"; accent "#FE8019"; }\n')
                    preview = command("theme", "preview", str(candidate))
                    self.assertEqual(preview["theme"]["appearance"], "dark")
                    self.assertEqual(command("theme", "get")["revision"], ready["revision"])
                    replace(shared, 'colors { accent "invalid"; }\n')
                    rejected = until(lambda: (snapshot := command("theme", "status"))["error"] and snapshot)
                    self.assertEqual(rejected["theme"], ready["theme"])
                    replace(shared, 'colors { accent "#8F5300"; }; future-token 1\n')
                    unknown = until(lambda: (snapshot := command("theme", "status"))["error"] and "future_token" in snapshot["error"] and snapshot)
                    self.assertEqual(unknown["theme"], ready["theme"])
                    replace(shared, 'colors { accent "#8F5300"; }\n')
                    repaired = until(lambda: (snapshot := command("theme", "get"))["error"] is None and snapshot["theme"]["requested_accent"] == "#8F5300" and snapshot["presented"] == snapshot["theme"] and snapshot)
                    time.sleep(.35)
                    self.assertEqual(command("theme", "get")["revision"], repaired["revision"])
                    replace(source, source.read_text().replace('file "themes/shared.kdl"', 'file "themes/missing.kdl"'))
                    until(lambda: command("theme", "get")["error"])
                    (shared.parent / "missing.kdl").write_text('material { style "translucent"; }; colors { accent "#3D7BE6"; }\n')
                    until(lambda: command("theme", "get")["error"] is None)
                    replace(source, source.read_text().replace('    mode ', '    accessibility { reduce-transparency #true; }\n    mode '))
                    solid = until(lambda: (snapshot := command("theme", "get"))["theme"]["accessibility"]["reduce_transparency"] and snapshot["presented"] == snapshot["theme"] and snapshot)
                    self.assertEqual(solid["theme"]["tokens"]["material"]["blur_radius"], 0)
                    self.assertEqual(solid["theme"]["tokens"]["material"]["style"], "solid")
                    imported = shared.parent / "family.kdl"
                    imported.write_text('theme { name "Imported"; dark { colors { accent "#CBA6F7"; }; }; }\n')
                    replace(source, 'theme { mode "light"; family "custom-demo"; split #false; custom-themes { custom-demo { file "themes/family.kdl"; }; }; }\n')
                    missing = until(lambda: (snapshot := command("theme", "get"))["fallback_note"] and "no light variant" in snapshot["fallback_note"] and snapshot)
                    self.assertTrue(any(family["id"] == "custom-demo" and family["light"] is None for family in missing["families"]))
                    command("theme", "mode", "dark")
                    applied = until(lambda: (snapshot := command("theme", "get"))["theme"]["requested_accent"] == "#CBA6F7" and snapshot["presented"] == snapshot["theme"] and snapshot)
                    self.assertIsNone(applied["fallback_note"])
                    replace(imported, 'theme { dark { colors { accent "invalid"; }; }; }\n')
                    rejected = until(lambda: (snapshot := command("theme", "get"))["error"] and snapshot)
                    self.assertEqual(rejected["theme"], applied["theme"])
                    replace(imported, 'theme { name "Repaired"; dark { colors { accent "#FE8019"; }; }; }\n')
                    until(lambda: (snapshot := command("theme", "get"))["error"] is None and snapshot["theme"]["requested_accent"] == "#FE8019")
                    replace(source, 'theme { mode "dark"; family "ferese-blue"; split #true; light { family "gruvbox"; }; dark { family "catppuccin"; }; }\n')
                    until(lambda: command("theme", "get")["theme"]["tokens"]["colors"]["surface_base"] == "#1E1E2E")
                    command("theme", "mode", "light")
                    until(lambda: command("theme", "get")["theme"]["tokens"]["colors"]["surface_base"] == "#FBF1C7")
                    if shutil.which("gsettings"):
                        subprocess.run(["gsettings", "set", "org.gnome.desktop.interface", "color-scheme", "prefer-dark"], env=ipc_environment(env), check=True, timeout=5)
                        replace(source, 'theme { mode "auto"; family "everforest"; schedule { source "system"; }; }\n')
                        until(lambda: (snapshot := command("theme", "get"))["mode"] == "auto" and snapshot["theme"]["appearance"] == "dark" and snapshot["fallback_note"] is None)
                        subprocess.run(["gsettings", "set", "org.gnome.desktop.interface", "color-scheme", "prefer-light"], env=ipc_environment(env), check=True, timeout=5)
                        until(lambda: (snapshot := command("theme", "get"))["theme"]["appearance"] == "light" and snapshot["presented"] == snapshot["theme"])
                    self.assertIsNone(compositor.poll())
                finally:
                    for child in reversed(children):
                        if child.poll() is None:
                            os.killpg(child.pid, signal.SIGTERM)
                        child.wait(timeout=5)
                        if child.stdout:
                            child.stdout.close()
                    if compositor.returncode not in (0, -signal.SIGTERM):
                        log.flush()
                        print((root / "compositor.log").read_text())


if __name__ == "__main__":
    unittest.main()
