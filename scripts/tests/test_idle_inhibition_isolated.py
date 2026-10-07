"""Test playback and window visibility on a private bus and nested desktop.

FERESE_TEST_IDLE_INHIBITION=1 python3 scripts/tests/test_idle_inhibition_isolated.py
Requires Wayland, PyGObject, dbus-run-session, cc and wayland-scanner.
FERESE_TEST_BINARY and FERESE_TEST_CTL can select release binaries.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest

REPO = Path(__file__).resolve().parents[2]
BINARY = REPO / os.environ.get("FERESE_TEST_BINARY", "target/debug/ferese")
CTL = REPO / os.environ.get("FERESE_TEST_CTL", "target/debug/feresectl")
NAME = "org.mpris.MediaPlayer2.ferese_test"
PATH = "/org/mpris/MediaPlayer2"
PLAYER = "org.mpris.MediaPlayer2.Player"
APP = "ferese.test.window-capture"


@unittest.skipUnless(os.environ.get("FERESE_TEST_IDLE_INHIBITION") == "1", "requires a Wayland host")
class IdleInhibition(unittest.TestCase):
    def test_private_playback_and_visibility(self):
        result = subprocess.run(["dbus-run-session", "--", sys.executable, __file__, "--private"],
                                capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


def private_checks():
    import gi
    gi.require_version("Gio", "2.0")
    from gi.repository import Gio, GLib

    loop = GLib.MainLoop()
    threading.Thread(target=loop.run, daemon=True).start()

    class FakePlayer:
        def __init__(self, status="Paused"):
            self.status = status
            self.connection = Gio.DBusConnection.new_for_address_sync(
                os.environ["DBUS_SESSION_BUS_ADDRESS"],
                Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
                None, None)
            node = Gio.DBusNodeInfo.new_for_xml(f'''<node>
                <interface name="org.mpris.MediaPlayer2"><property name="DesktopEntry" type="s" access="read"/></interface>
                <interface name="{PLAYER}"><property name="PlaybackStatus" type="s" access="read"/></interface>
                </node>''')
            self.registrations = [self.connection.register_object(PATH, interface, None, self.property, None)
                                  for interface in node.interfaces]
            self.connection.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                                      "RequestName", GLib.Variant("(su)", (NAME, 0)), None,
                                      Gio.DBusCallFlags.NONE, 2000, None)

        def property(self, connection, sender, path, interface, name):
            return GLib.Variant("s", self.status if name == "PlaybackStatus" else APP)

        def set(self, status, invalidated=False):
            self.status = status
            changed = {} if invalidated else {"PlaybackStatus": GLib.Variant("s", status)}
            fields = ["PlaybackStatus"] if invalidated else []
            self.connection.emit_signal(None, PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged",
                                        GLib.Variant("(sa{sv}as)", (PLAYER, changed, fields)))
            self.connection.flush_sync(None)

        def close(self):
            self.connection.close_sync(None)

    processes = []
    player = FakePlayer()
    try:
        with tempfile.TemporaryDirectory(prefix="ferese-idle-") as directory:
            root = Path(directory)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese/config.kdl"
            config.parent.mkdir(parents=True)
            base = f'animations {{ reduced-motion #true; }}\nwindow-rule app-id="{APP}" floating=#true\n'
            config.write_text(base)
            protocols = subprocess.check_output(["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True).strip()
            xml = str(Path(protocols) / "stable/xdg-shell/xdg-shell.xml")
            for kind, output in [("client-header", "xdg-shell-client-protocol.h"), ("private-code", "xdg-shell-protocol.c")]:
                subprocess.run(["wayland-scanner", kind, xml, str(root / output)], check=True)
            flags = subprocess.check_output(["pkg-config", "--cflags", "--libs", "wayland-client"], text=True).split()
            subprocess.run(["cc", "-Wall", "-Wextra", "-I", str(root), str(REPO / "scripts/tests/fixtures/window-capture.c"),
                            str(root / "xdg-shell-protocol.c"), "-o", str(root / "client"), *flags], check=True)
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
            env = dict(os.environ, WAYLAND_DISPLAY=str(display), XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"))
            for variable in ["WAYLAND_SOCKET", "FERESE_SOCKET", "FERESE_SHELL_CONTROL_SOCKET"]:
                env.pop(variable, None)

            with (root / "compositor.log").open("w") as log:
                try:
                    compositor = subprocess.Popen([str(BINARY), "--backend=nested"], env=env, stdout=log, stderr=log, start_new_session=True)
                    processes.append(compositor)

                    def wait(check, message):
                        deadline = time.monotonic() + 8
                        while time.monotonic() < deadline:
                            if compositor.poll() is not None:
                                raise AssertionError((root / "compositor.log").read_text())
                            if check():
                                return
                            time.sleep(0.02)
                        raise AssertionError(message + "\n" + (root / "compositor.log").read_text())

                    wait(lambda: (runtime / "ferese/control.sock").exists(), "compositor startup")
                    env["WAYLAND_DISPLAY"] = str(next(path for path in runtime.glob("wayland-*") if not path.name.endswith(".lock")))

                    def call(*args):
                        return json.loads(subprocess.check_output([str(CTL), "-j", *args], env=env, timeout=5))

                    def inhibited(expected):
                        def matches():
                            state = call("get-idle-inhibition")
                            return state["automatic"] == expected and state["inhibited"] == expected

                        wait(matches, f"idle inhibition != {expected}")

                    wait(lambda: len(call("get-idle-inhibition")["players"]) == 1, "MPRIS discovery")
                    client = subprocess.Popen([str(root / "client")], env=env, stdout=log, stderr=log, start_new_session=True)
                    processes.append(client)
                    wait(lambda: len(call("get-windows")) == 1, "window map")
                    call("toggle-fullscreen")
                    inhibited(False)
                    player.set("Playing")
                    inhibited(True)
                    player.set("Paused")
                    inhibited(False)
                    player.set("Playing", invalidated=True)
                    inhibited(True)
                    player.set("Stopped")
                    inhibited(False)
                    player.set("Playing")
                    inhibited(True)
                    call("workspace", "2")
                    inhibited(False)
                    call("workspace", "1")
                    inhibited(True)

                    player.close()
                    inhibited(False)
                    player = FakePlayer("Paused")
                    wait(lambda: len(call("get-idle-inhibition")["players"]) == 1, "replacement discovery")
                    inhibited(False)
                    player.set("Playing")
                    inhibited(True)
                    ambiguous = subprocess.Popen([str(root / "client"), "cover"], env=env,
                                                 stdout=log, stderr=log, start_new_session=True)
                    processes.append(ambiguous)
                    wait(lambda: len(call("get-windows")) == 2, "second player window")
                    inhibited(False)
                    os.killpg(ambiguous.pid, signal.SIGTERM)
                    ambiguous.wait(timeout=5)
                    wait(lambda: len(call("get-windows")) == 1, "ambiguous window closure")
                    inhibited(True)
                    config.write_text(base + f'window-rule app-id="{APP}" idle-inhibit="none"\n')
                    call("reload-config")
                    inhibited(False)
                    config.write_text(base)
                    call("reload-config")
                    inhibited(True)
                    call("toggle-fullscreen")
                    inhibited(False)

                    config.write_text(base + f'idle-inhibit {{ fullscreen-playback #false; }}\nwindow-rule app-id="{APP}" idle-inhibit="visible"\n')
                    call("reload-config")
                    inhibited(True)
                    player.set("Stopped")
                    inhibited(True)
                    call("workspace", "2")
                    inhibited(False)
                    call("workspace", "1")
                    inhibited(True)
                    config.write_text(base + 'idle-inhibit { fullscreen-playback #false; }\n')
                    call("reload-config")
                    inhibited(False)
                    config.write_text(base + f'window-rule app-id="{APP}" idle-inhibit="visible"\n')
                    call("reload-config")
                    inhibited(True)
                    os.killpg(client.pid, signal.SIGTERM)
                    client.wait(timeout=5)
                    wait(lambda: not call("get-windows"), "closed window remained registered")
                    inhibited(False)
                finally:
                    for process in reversed(processes):
                        if process.poll() is None:
                            os.killpg(process.pid, signal.SIGTERM)
                            process.wait(timeout=5)
    finally:
        player.close()
        loop.quit()


if __name__ == "__main__":
    if "--private" in sys.argv:
        private_checks()
    else:
        unittest.main()
