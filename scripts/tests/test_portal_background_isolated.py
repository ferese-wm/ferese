"""Background portal against a private bus and a fake compositor event stream.

FERESE_TEST_PORTAL_BACKGROUND=1 python3 -m unittest discover -s scripts/tests -p test_portal_background_isolated.py
No Wayland session or public D-Bus service is used.
"""
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import threading
import time
import unittest


@unittest.skipUnless(os.environ.get("FERESE_TEST_PORTAL_BACKGROUND") == "1", "private Background portal test is opt-in")
class BackgroundPortal(unittest.TestCase):
    def test_disconnect_clears_apps_signals_once_and_reconnect_replaces_state(self):
        import gi
        from gi.repository import Gio, GLib

        repo = Path(__file__).resolve().parents[2]
        binary = repo / os.environ.get("FERESE_TEST_PORTAL_BINARY", "target/debug/xdg-desktop-portal-ferese")
        with tempfile.TemporaryDirectory(prefix="ferese-background-") as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            address_file = root / "control.sock"
            listener = socket.socket(socket.AF_UNIX)
            listener.bind(str(address_file))
            listener.listen()
            listener.settimeout(.1)
            stop = threading.Event()
            streams = []
            failures = []

            def read_exact(peer, count):
                data = b""
                while len(data) < count:
                    part = peer.recv(count - len(data))
                    if not part:
                        raise EOFError("IPC peer closed")
                    data += part
                return data

            def send(peer, value):
                body = json.dumps(value).encode()
                peer.sendall(struct.pack(">I", len(body)) + body)

            def accept():
                while not stop.is_set():
                    try:
                        peer, _ = listener.accept()
                    except socket.timeout:
                        continue
                    except OSError:
                        return
                    try:
                        peer.settimeout(2)
                        size = struct.unpack(">I", read_exact(peer, 4))[0]
                        self.assertLessEqual(size, 1024 * 1024)
                        request = json.loads(read_exact(peer, size))
                        if request["command"] != "event-stream":
                            send(peer, {"version": 1, "id": request["id"], "error": {"code": "unsupported", "message": "test peer"}})
                            peer.close()
                            continue
                        send(peer, {"version": 1, "id": request["id"], "result": {"version": 1}})
                        streams.append(peer)
                    except Exception as error:
                        peer.close()
                        failures.append(error)

            worker = threading.Thread(target=accept, daemon=True)
            worker.start()
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), FERESE_SOCKET=str(address_file),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_CACHE_HOME=str(root / "cache"))
            with (root / "portal.log").open("w+") as log:
                bus = subprocess.Popen(["dbus-daemon", "--session", "--nofork", "--print-address=1"],
                                       stdout=subprocess.PIPE, stderr=log, text=True)
                env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                bus.stdout.close()
                connection = Gio.DBusConnection.new_for_address_sync(env["DBUS_SESSION_BUS_ADDRESS"],
                    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
                dbus = "org.freedesktop.DBus"
                # GetAppState is restricted to the frontend's bus owner.
                connection.call_sync(dbus, "/org/freedesktop/DBus", dbus, "RequestName",
                    GLib.Variant("(su)", ("org.freedesktop.portal.Desktop", 0)), GLib.VariantType("(u)"),
                    Gio.DBusCallFlags.NONE, 2000, None)
                backend = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=log)
                name, path, interface = "org.freedesktop.impl.portal.desktop.ferese", "/org/freedesktop/portal/desktop", "org.freedesktop.impl.portal.Background"
                changes = []
                subscription = connection.signal_subscribe(name, interface, "RunningApplicationsChanged", path, None,
                    Gio.DBusSignalFlags.NONE, lambda *args: changes.append(True))

                def until(predicate, timeout=5):
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        while GLib.MainContext.default().iteration(False):
                            pass
                        self.assertFalse(failures, str(failures))
                        self.assertIsNone(backend.poll(), (root / "portal.log").read_text())
                        value = predicate()
                        if value:
                            return value
                        time.sleep(.01)
                    self.fail((root / "portal.log").read_text())

                def state():
                    return connection.call_sync(name, path, interface, "GetAppState", GLib.Variant("()", ()),
                        GLib.VariantType("(a{sv})"), Gio.DBusCallFlags.NO_AUTO_START, 2000, None).unpack()[0]

                def snapshot(peer, app):
                    send(peer, {"version": 1, "generation": 0, "last": True, "type": "snapshot", "desktop": {
                        "outputs": [], "workspaces": [], "windows": [{
                            "id": 1, "app_id": app, "title": app, "focused": True,
                            "workspace": 1, "floating": False, "fullscreen": False}],
                        "focus": {"window": 1}, "config": {"revision": 0, "error": None}, "theme": None,
                        "lock": {"phase": "unlocked", "sleeping": False}}})

                try:
                    until(lambda: streams)
                    snapshot(streams[0], "org.test.Gone")
                    until(lambda: state().get("org.test.Gone") == 2 and len(changes) == 1)
                    streams[0].shutdown(socket.SHUT_RDWR)
                    streams[0].close()
                    # Keep the replacement subscription idle until its snapshot.
                    # The previous desktop must already be empty during this gap.
                    until(lambda: not state() and len(changes) == 2)
                    until(lambda: len(streams) >= 2)
                    self.assertEqual(state(), {})
                    self.assertEqual(len(changes), 2)
                    snapshot(streams[1], "org.test.New")
                    until(lambda: state() == {"org.test.New": 2} and len(changes) == 3)
                    streams[1].shutdown(socket.SHUT_RDWR)
                    streams[1].close()
                    until(lambda: not state() and len(changes) == 4)
                    # A second disconnect before receiving any snapshot is a no-op.
                    until(lambda: len(streams) >= 3)
                    streams[2].shutdown(socket.SHUT_RDWR)
                    streams[2].close()
                    until(lambda: len(streams) >= 4)
                    self.assertEqual(state(), {})
                    self.assertEqual(len(changes), 4)
                finally:
                    backend.terminate()
                    backend.wait(timeout=5)
                    connection.signal_unsubscribe(subscription)
                    connection.close_sync(None)
                    bus.terminate()
                    bus.wait(timeout=5)
                    stop.set()
                    listener.close()
                    worker.join(timeout=3)
                    for peer in streams:
                        peer.close()
