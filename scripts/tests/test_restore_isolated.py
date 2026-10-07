"""Check restored monitor sharing on a private bus and nested compositor.

FERESE_TEST_RESTORE=1 python3 scripts/tests/test_restore_isolated.py
The IPC fixture supplies synthetic EDID identities for the nested display.
No host permission store, compositor configuration or portal service is changed.
"""
import json
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest

from test_inhibit_isolated import IPC

REPO = Path(__file__).resolve().parents[2]
NAME = "org.freedesktop.impl.portal.desktop.ferese"
PATH = "/org/freedesktop/portal/desktop"
INTERFACE = "org.freedesktop.impl.portal.ScreenCast"


@unittest.skipUnless(os.environ.get("FERESE_TEST_RESTORE") == "1", "requires a Wayland host and PipeWire")
class RestoreIntegration(unittest.TestCase):
    def test_private_bus_restore_lifetimes(self):
        result = subprocess.run(["dbus-run-session", "--", sys.executable, __file__, "--private"], capture_output=True, text=True, timeout=70)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


def private_test():
    import gi
    gi.require_version("Gio", "2.0")
    gi.require_version("Gst", "1.0")
    from gi.repository import Gio, GLib, Gst
    Gst.init(None)
    processes = []
    stop = threading.Event()
    proxy = None

    def wait_for(check, message, seconds=10):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            while GLib.MainContext.default().pending():
                GLib.MainContext.default().iteration(False)
            if check():
                return
            time.sleep(0.02)
        raise AssertionError(message)

    def proxy_connection(client, destination):
        with client:
            upstream = IPC(destination)
            try:
                def read(size):
                    result = b""
                    while len(result) < size:
                        chunk = client.recv(size - len(result))
                        if not chunk:
                            raise EOFError()
                        result += chunk
                    return result
                while not stop.is_set():
                    request = json.loads(read(struct.unpack(">I", read(4))[0]))
                    result = upstream.call(request["command"], request.get("args"))
                    if request["command"] == "get-outputs":
                        for output in result:
                            output["identity"] = "drm-edid:test-" + output["name"]
                    reply = json.dumps({"version": 1, "id": request["id"], "result": result}).encode()
                    client.sendall(struct.pack(">I", len(reply)) + reply)
            except (EOFError, OSError):
                pass
            finally:
                upstream.close()

    with tempfile.TemporaryDirectory(prefix="ferese-restore-") as directory:
        root = Path(directory)
        runtime = root / "compositor"
        runtime.mkdir(mode=0o700)
        backend_runtime = root / "portal"
        (backend_runtime / "ferese").mkdir(parents=True, mode=0o700)
        config = root / "config/ferese/config.kdl"
        config.parent.mkdir(parents=True)
        config.write_text("status { keybinding-guide #false; }\n")
        host_runtime = os.environ["XDG_RUNTIME_DIR"]
        display = Path(os.environ["WAYLAND_DISPLAY"])
        if not display.is_absolute():
            display = Path(host_runtime) / display
        env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"), WAYLAND_DISPLAY=str(display), PIPEWIRE_RUNTIME_DIR=host_runtime, FERESE_ENABLE_SCREENCOPY="1", DBUS_SYSTEM_BUS_ADDRESS=os.environ["DBUS_SESSION_BUS_ADDRESS"])
        env.pop("WAYLAND_SOCKET", None)
        log = (root / "test.log").open("w")
        try:
            compositor = subprocess.Popen([str(REPO / "target/debug/ferese"), "--backend=nested"], env=ipc_environment(env), stdout=log, stderr=log, start_new_session=True)
            processes.append(compositor)
            wait_for(lambda: ipc_socket(runtime).is_socket(), "Nested compositor did not start")
            destination = ipc_socket(runtime)
            admin = IPC(destination)
            outputs = admin.call("get-outputs")
            name = next(output["name"] for output in outputs if output["enabled"])
            admin.close()
            displays = lambda: [path for path in runtime.glob("wayland-*") if not path.name.endswith(".lock")]
            wait_for(displays, "Missing nested Wayland socket")
            env["WAYLAND_DISPLAY"] = str(displays()[0])
            env["XDG_RUNTIME_DIR"] = str(backend_runtime)
            proxy = socket.socket(socket.AF_UNIX)
            proxy.bind(str(backend_runtime / "ferese/control.sock"))
            proxy.listen()
            proxy.settimeout(0.1)
            def accept():
                while not stop.is_set():
                    try:
                        client, _ = proxy.accept()
                    except socket.timeout:
                        continue
                    except OSError:
                        break
                    threading.Thread(target=proxy_connection, args=(client, destination), daemon=True).start()
            threading.Thread(target=accept, daemon=True).start()
            bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
            bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", GLib.Variant("(su)", ("org.freedesktop.portal.Desktop", 0)), None, Gio.DBusCallFlags.NONE, 2000, None)
            backend = subprocess.Popen([str(REPO / "target/debug/xdg-desktop-portal-ferese")], env=ipc_environment(env), stdout=log, stderr=log, start_new_session=True)
            processes.append(backend)
            def call(method, params=None, path=PATH, interface=INTERFACE):
                return bus.call_sync(NAME, path, interface, method, params, None, Gio.DBusCallFlags.NO_AUTO_START, 12000, None).unpack()
            def ready():
                try:
                    call("Introspect", interface="org.freedesktop.DBus.Introspectable")
                    return True
                except GLib.Error:
                    return False
            wait_for(ready, "Portal backend did not start")
            data = {"app": "org.test.Share", "cursor": False, "mode": 1, "monitors": [{"name": name, "identity": "drm-edid:test-" + name}]}
            def create(token, app="org.test.Share", saved=data):
                session = PATH + "/session/test/" + token
                handle = PATH + "/request/test/" + token
                assert call("CreateSession", GLib.Variant("(oosa{sv})", (handle, session, app, {}))) == (0, {})
                options = {"persist_mode": GLib.Variant("u", 2), "restore_data": GLib.Variant("(suv)", ("Ferese", 1, GLib.Variant("s", json.dumps(saved))))}
                assert call("SelectSources", GLib.Variant("(oosa{sv})", (handle, session, app, options))) == (0, {})
                return session, handle
            def helpers():
                return [pid for children in Path(f"/proc/{backend.pid}/task").glob("*/children") for pid in children.read_text().split()]
            session, handle = create("restore")
            response, result = call("Start", GLib.Variant("(oossa{sv})", (handle, session, "org.test.Share", "", {})))
            assert response == 0 and result["persist_mode"] == 1, (response, result)
            assert result["restore_data"][:2] == ("Ferese", 1), result
            assert json.loads(result["restore_data"][2])["mode"] == 1
            assert len(result["streams"]) == 1
            assert all("--picker" not in Path(f"/proc/{pid}/cmdline").read_text() for pid in helpers()), "Restored selection opened a picker"
            node = result["streams"][0][0]
            pipeline = Gst.parse_launch(f"pipewiresrc path={node} num-buffers=5 ! video/x-raw,format=BGRx ! fakesink sync=false")
            try:
                pipeline.set_state(Gst.State.PLAYING)
                message = pipeline.get_bus().timed_pop_filtered(8 * Gst.SECOND, Gst.MessageType.EOS | Gst.MessageType.ERROR)
                assert message and message.type == Gst.MessageType.EOS, "Restored stream did not deliver video"
            finally:
                pipeline.set_state(Gst.State.NULL)
            call("Close", path=session, interface="org.freedesktop.impl.portal.Session")
            wait_for(lambda: not helpers(), "Closing restored session left capture children")
            print("PASS: wire restore, transient duration retained, video delivery and Close cleanup", flush=True)

            def fallback(token, app="org.test.Share", saved=data):
                session, handle = create(token, app, saved)
                replies = []
                def done(connection, result):
                    try:
                        replies.append(connection.call_finish(result).unpack())
                    except GLib.Error as error:
                        replies.append(error)
                bus.call(NAME, PATH, INTERFACE, "Start", GLib.Variant("(oossa{sv})", (handle, session, app, "", {})), None, Gio.DBusCallFlags.NO_AUTO_START, 12000, None, done)
                wait_for(lambda: any("--picker" in Path(f"/proc/{pid}/cmdline").read_text() for pid in helpers()), "Invalid restore did not require picker")
                call("Close", path=handle, interface="org.freedesktop.impl.portal.Request")
                wait_for(lambda: replies and not helpers(), "Cancelled fallback left helpers")
                assert replies == [(1, {})], replies
            fallback("wrongapp", app="org.other.App")
            stale = dict(data, monitors=[{"name": name, "identity": "drm-edid:other-device"}])
            fallback("replacement", saved=stale)
            print("PASS: other-app and changed-device restore require fresh consent", flush=True)

            mismatch = subprocess.Popen([str(REPO / "target/debug/xdg-desktop-portal-ferese"), "--stream", name, "hidden", str(2**32 - 1)], env=ipc_environment(env), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
            processes.append(mismatch)
            mismatch.wait(timeout=8)
            error = mismatch.stderr.read()
            mismatch.stdin.close()
            assert mismatch.returncode != 0 and "replaced" in error, error
            session, handle = create("frontendloss")
            assert call("Start", GLib.Variant("(oossa{sv})", (handle, session, "org.test.Share", "", {})))[0] == 0
            bus.close_sync(None)
            wait_for(lambda: not helpers(), "Frontend loss did not revoke restored capture")
            print("PASS: output-generation mismatch and frontend disconnect revoke capture", flush=True)
        except Exception:
            print((root / "test.log").read_text(), file=sys.stderr)
            raise
        finally:
            stop.set()
            if proxy:
                proxy.close()
            for process in reversed(processes):
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait()
            log.close()


if __name__ == "__main__":
    if "--private" in sys.argv:
        private_test()
    else:
        unittest.main()
