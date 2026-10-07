"""Exercise real inhibitor FD lifetimes and session handshake on an isolated bus.

FERESE_TEST_INHIBIT=1 python3 scripts/tests/test_inhibit_isolated.py
Uses a nested compositor and mock logind; never suspends or ends the host session.
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

REPO = Path(__file__).resolve().parents[2]
NAME = "org.freedesktop.impl.portal.desktop.ferese"
PATH = "/org/freedesktop/portal/desktop"
INTERFACE = "org.freedesktop.impl.portal.Inhibit"


@unittest.skipUnless(os.environ.get("FERESE_TEST_INHIBIT") == "1", "requires a Wayland host")
class InhibitIntegration(unittest.TestCase):
    def test_private_bus_lifetimes_and_query_end(self):
        result = subprocess.run(["dbus-run-session", "--", sys.executable, __file__, "--private"], capture_output=True, text=True, timeout=40)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class IPC:
    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(3)
        self.socket.connect(str(path))

    def call(self, command, args=None):
        request = json.dumps({"version": 1, "id": 1, "type": "command", "command": command, "args": args or {}}).encode()
        self.socket.sendall(struct.pack(">I", len(request)) + request)
        def read(size):
            result = b""
            while len(result) < size:
                chunk = self.socket.recv(size - len(result))
                if not chunk:
                    raise EOFError("IPC connection closed")
                result += chunk
            return result
        response = json.loads(read(struct.unpack(">I", read(4))[0]))
        if response.get("error"):
            raise ValueError(response["error"])
        return response["result"]

    def close(self):
        self.socket.close()


def private_test():
    from gi.repository import Gio, GLib
    processes = []
    pipes = []
    pipe_lock = threading.Lock()
    ready = threading.Event()
    login_loop = None
    login_connection = None
    manager_xml = """<node><interface name="org.freedesktop.login1.Manager">
      <method name="Inhibit"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="h" direction="out"/></method>
      <method name="ListInhibitors"><arg type="a(ssssuu)" direction="out"/></method>
      <signal name="PrepareForShutdown"><arg type="b"/></signal>
    </interface></node>"""

    def logind():
        nonlocal login_loop, login_connection
        context = GLib.MainContext.new()
        context.push_thread_default()
        login_connection = Gio.DBusConnection.new_for_address_sync(os.environ["DBUS_SESSION_BUS_ADDRESS"], Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
        login_connection.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", GLib.Variant("(su)", ("org.freedesktop.login1", 0)), None, Gio.DBusCallFlags.NONE, 2000, None)
        def method(connection, sender, object_path, interface, name, params, invocation):
            if name == "Inhibit":
                what, app, reason, mode = params.unpack()
                reader, writer = os.pipe2(os.O_CLOEXEC | os.O_NONBLOCK)
                descriptors = Gio.UnixFDList.new()
                index = descriptors.append(writer)
                with pipe_lock:
                    pipes.append((reader, what, app, reason, mode))
                invocation.return_value_with_unix_fd_list(GLib.Variant("(h)", (index,)), descriptors)
                os.close(writer)
            else:
                rows = []
                with pipe_lock:
                    for reader, what, app, reason, mode in pipes:
                        try:
                            if os.read(reader, 1) == b"":
                                continue
                        except BlockingIOError:
                            pass
                        rows.append((what, app, reason, mode, os.getuid(), os.getpid()))
                invocation.return_value(GLib.Variant("(a(ssssuu))", (rows,)))
        login_connection.register_object("/org/freedesktop/login1", Gio.DBusNodeInfo.new_for_xml(manager_xml).interfaces[0], method, None, None)
        login_loop = GLib.MainLoop.new(context, False)
        ready.set()
        login_loop.run()
        context.pop_thread_default()

    thread = threading.Thread(target=logind, daemon=True)
    thread.start()
    assert ready.wait(3), "Mock logind did not start"
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(interface, method, params=None, signature=None, path=PATH):
        return bus.call_sync(NAME, path, interface, method, params, GLib.VariantType.new(signature) if signature else None, Gio.DBusCallFlags.NO_AUTO_START, 5000, None).unpack()

    def wait_for(check, message):
        deadline = time.monotonic() + 4
        while time.monotonic() < deadline:
            while GLib.MainContext.default().pending():
                GLib.MainContext.default().iteration(False)
            if check():
                return
            time.sleep(0.01)
        raise AssertionError(message)

    def active_fd(index):
        with pipe_lock:
            try:
                return os.read(pipes[index][0], 1) != b""
            except BlockingIOError:
                return True

    with tempfile.TemporaryDirectory(prefix="ferese-inhibit-") as directory:
        root = Path(directory)
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        config = root / "config/ferese/config.kdl"
        config.parent.mkdir(parents=True)
        config.write_text("status { keybinding-guide #false; }\n")
        display = Path(os.environ["WAYLAND_DISPLAY"])
        if not display.is_absolute():
            display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
        env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(root / "config"), WAYLAND_DISPLAY=str(display), DBUS_SYSTEM_BUS_ADDRESS=os.environ["DBUS_SESSION_BUS_ADDRESS"])
        env.pop("WAYLAND_SOCKET", None)
        log = (root / "test.log").open("w")
        try:
            compositor = subprocess.Popen([str(REPO / "target/debug/ferese"), "--backend=nested"], env=ipc_environment(env), stdout=log, stderr=log, start_new_session=True)
            processes.append(compositor)
            deadline = time.monotonic() + 10
            while not (ipc_socket(runtime)).exists():
                assert compositor.poll() is None and time.monotonic() < deadline, (root / "test.log").read_text()
                time.sleep(0.02)
            backend = subprocess.Popen([str(REPO / "target/debug/xdg-desktop-portal-ferese")], env=ipc_environment(env), stdout=log, stderr=log, start_new_session=True)
            processes.append(backend)
            deadline = time.monotonic() + 10
            while True:
                try:
                    call("org.freedesktop.DBus.Introspectable", "Introspect", signature="(s)")
                    break
                except GLib.Error:
                    assert backend.poll() is None and time.monotonic() < deadline, (root / "test.log").read_text()
                    time.sleep(0.02)
            request = PATH + "/request/test/inhibit"
            try:
                call(INTERFACE, "Inhibit", GLib.Variant("(ossua{sv})", (request, "test.media", "", 8, {})))
                raise AssertionError("Untrusted inhibition was accepted")
            except GLib.Error as error:
                assert "AccessDenied" in str(error)
            bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", GLib.Variant("(su)", ("org.freedesktop.portal.Desktop", 0)), None, Gio.DBusCallFlags.NONE, 2000, None)
            admin = IPC(ipc_socket(runtime))
            def inhibit(path, flags):
                call(INTERFACE, "Inhibit", GLib.Variant("(ossua{sv})", (path, "test.media", "", flags, {"reason": GLib.Variant("s", "Playing media")})))
            inhibit(request, 8)
            assert active_fd(0)
            assert admin.call("get-session-state")["inhibitors"][0]["flags"] == 8
            call("org.freedesktop.impl.portal.Request", "Close", path=request)
            wait_for(lambda: not active_fd(0) and not admin.call("get-session-state")["inhibitors"], "Close retained inhibition")
            session = PATH + "/session/test/monitor"
            changes = []
            closed = []
            bus.signal_subscribe(NAME, INTERFACE, "StateChanged", PATH, None, Gio.DBusSignalFlags.NONE, lambda *args: changes.append(args[-1].unpack()[1]))
            bus.signal_subscribe(NAME, "org.freedesktop.impl.portal.Session", "Closed", session, None, Gio.DBusSignalFlags.NONE, lambda *args: closed.append(True))
            assert call(INTERFACE, "CreateMonitor", GLib.Variant("(ooss)", (PATH + "/request/test/monitor", session, "test.media", "")), "(u)") == (0,)
            wait_for(lambda: any(value["session-state"] == 1 for value in changes), "Missing initial Running")
            assert call("org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", ("org.freedesktop.impl.portal.Session", "version")), "(v)", session) == (1,)
            query = IPC(ipc_socket(runtime))
            state = query.call("begin-session-end")
            assert state["query-ready"] is False
            wait_for(lambda: any(value["session-state"] == 2 for value in changes), "Missing QueryEnd")
            call(INTERFACE, "QueryEndResponse", GLib.Variant("(o)", (session,)))
            assert admin.call("get-session-state")["query-ready"] is True
            approved = state["inhibitor-revision"]
            inhibit(request, 1)
            try:
                query.call("commit-session-end", {"token": state["query-token"], "inhibitor-revision": approved, "force": True})
                raise AssertionError("Late inhibitor bypassed confirmation")
            except ValueError:
                pass
            current = admin.call("get-session-state")
            query.call("validate-session-end", {"token": state["query-token"], "inhibitor-revision": current["inhibitor-revision"], "force": True})
            assert admin.call("get-session-state")["session-state"] == 2
            query.call("commit-session-end", {"token": state["query-token"], "inhibitor-revision": current["inhibitor-revision"], "force": True})
            wait_for(lambda: any(value["session-state"] == 3 for value in changes), "Missing Ending")
            query.call("cancel-session-end", {"token": state["query-token"]})
            wait_for(lambda: changes[-1]["session-state"] == 1, "Missing cancelled end state")
            query.close()
            call("org.freedesktop.impl.portal.Session", "Close", path=session)
            wait_for(lambda: len(closed) == 1, "Missing Closed")
            count = len(changes)
            login_connection.emit_signal(None, "/org/freedesktop/login1", "org.freedesktop.login1.Manager", "PrepareForShutdown", GLib.Variant("(b)", (True,)))
            wait_for(lambda: admin.call("get-session-state")["session-state"] == 3, "Missing external shutdown state")
            time.sleep(0.1)
            while GLib.MainContext.default().pending():
                GLib.MainContext.default().iteration(False)
            assert len(changes) == count, "StateChanged appeared after Closed"
            compositor.terminate()
            compositor.wait(timeout=5)
            wait_for(lambda: not active_fd(1), "Compositor death retained logind inhibitor FD")
            admin.close()
            print("Inhibit FD, monitor, ACK, late consent and disconnect checks passed")
        finally:
            for process in reversed(processes):
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=5)
            log.close()
    login_loop.quit()
    thread.join(timeout=2)
    for pipe, *_ in pipes:
        os.close(pipe)


if __name__ == "__main__":
    if "--private" in sys.argv:
        private_test()
    else:
        unittest.main()
