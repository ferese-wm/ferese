"""Opt-in portal integration test; only captures a temporary nested compositor.

FERESE_TEST_PORTAL=1 python3 scripts/tests/test_portal_isolated.py
Add FERESE_TEST_PORTAL_CONSENT=1 to exercise the actual picker interactively.
Requires built binaries, Python GI/GStreamer (pipewiresrc), dbus-daemon,
xdg-desktop-portal and bwrap. Host PAM and D-Bus activation are never modified.
"""
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile
import time
import unittest
from session_socket import ipc_environment


@unittest.skipUnless(os.environ.get("FERESE_TEST_PORTAL") == "1", "nested portal test is opt-in")
class PortalTest(unittest.TestCase):
    def test_portal_capture_cancel_and_lock(self):
        import gi
        gi.require_version("Gst", "1.0")
        from gi.repository import Gio, GLib, Gst
        Gst.init(None)
        repo = Path(__file__).resolve().parents[2]
        binary = repo / os.environ.get("FERESE_TEST_PORTAL_BINARY", "target/debug/xdg-desktop-portal-ferese")
        compositor_binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/release/ferese")
        processes, logs = [], []
        with tempfile.TemporaryDirectory(prefix="ferese-portal-test-") as tmp:
            root = Path(tmp)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            (config / "config.kdl").write_text('window-rule app-id="dev.ferese.ScreenShare" floating=#true\n')
            display = Path(os.environ["WAYLAND_DISPLAY"])
            if not display.is_absolute():
                display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(root / "config"),
                       WAYLAND_DISPLAY=str(display), FERESE_ENABLE_SCREENCOPY="1",
                       PIPEWIRE_RUNTIME_DIR=os.environ["XDG_RUNTIME_DIR"], XDG_CURRENT_DESKTOP="Ferese",
                       XDG_DESKTOP_PORTAL_DIR=str(repo / "packaging/portal"))

            def launch(name, command, **kwargs):
                log = (root / (name + ".log")).open("w")
                logs.append(log)
                child = subprocess.Popen(command, env=ipc_environment(env), stderr=log,
                                         stdout=kwargs.pop("stdout", log), start_new_session=True, **kwargs)
                processes.append(child)
                return child

            def until(predicate, seconds=10):
                deadline = time.monotonic() + seconds
                while time.monotonic() < deadline:
                    if predicate():
                        return
                    while GLib.MainContext.default().iteration(False):
                        pass
                    time.sleep(.02)
                self.fail("Timed out waiting for test condition")

            def read_node(worker):
                with selectors.DefaultSelector() as selector:
                    selector.register(worker.stdout, selectors.EVENT_READ)
                    self.assertTrue(selector.select(10), "stream did not publish a node")
                return json.loads(worker.stdout.readline())["node"]

            def consume(node, fd=None):
                source = f'pipewiresrc path={node} num-buffers=15'
                if fd is not None:
                    source += f' fd={fd}'
                pipeline = Gst.parse_launch(source + ' ! video/x-raw,format=BGRx ! appsink name=sink sync=false emit-signals=true')
                sizes, timestamps = [], []
                def sample(sink):
                    buffer = sink.emit("pull-sample").get_buffer()
                    sizes.append(buffer.get_size())
                    timestamps.append(buffer.pts)
                    return Gst.FlowReturn.OK
                pipeline.get_by_name("sink").connect("new-sample", sample)
                try:
                    pipeline.set_state(Gst.State.PLAYING)
                    message = pipeline.get_bus().timed_pop_filtered(15 * Gst.SECOND, Gst.MessageType.EOS | Gst.MessageType.ERROR)
                    self.assertIsNotNone(message, "PipeWire consumer timed out")
                    self.assertEqual(message.type, Gst.MessageType.EOS)
                    self.assertEqual(len(sizes), 15)
                    self.assertGreater(min(sizes), 0)
                    self.assertTrue(all(b > a for a, b in zip(timestamps, timestamps[1:])), timestamps)
                    self.assertGreater(timestamps[-1] - timestamps[0], Gst.SECOND // 5)
                finally:
                    pipeline.set_state(Gst.State.NULL)

            try:
                launch("compositor", [str(compositor_binary), "--backend", "nested"])
                sockets = lambda: [p for p in runtime.glob("wayland-*") if not p.name.endswith(".lock")]
                until(sockets)
                env["WAYLAND_DISPLAY"] = str(sockets()[0])
                env = ipc_environment(env)
                # Private bus, including the real portal frontend and permission store.
                bus = launch("bus", ["dbus-daemon", "--session", "--nofork", "--print-address=1"], stdout=subprocess.PIPE)
                env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().decode().strip()
                connection = Gio.DBusConnection.new_for_address_sync(env["DBUS_SESSION_BUS_ADDRESS"],
                    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
                backend = launch("backend", [str(binary)])
                # The frontend can D-Bus-activate an installed backend if it
                # starts before this test's binary owns its service name.
                def backend_ready():
                    bus_name = "org.freedesktop.DBus"
                    impl_name = "org.freedesktop.impl.portal.desktop.ferese"
                    owner = connection.call_sync(bus_name, "/org/freedesktop/DBus", bus_name,
                        "NameHasOwner", GLib.Variant("(s)", (impl_name,)), GLib.VariantType("(b)"),
                        Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
                    if not owner:
                        return False

                    pid = connection.call_sync(bus_name, "/org/freedesktop/DBus", bus_name,
                        "GetConnectionUnixProcessID", GLib.Variant("(s)", (impl_name,)), GLib.VariantType("(u)"),
                        Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
                    return pid == backend.pid

                until(backend_ready)
                frontend = launch("frontend", [os.environ.get("FERESE_TEST_PORTAL_FRONTEND", "/usr/libexec/xdg-desktop-portal")])
                name, path, iface = "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.ScreenCast"
                responses = {}
                connection.signal_subscribe(name, "org.freedesktop.portal.Request", "Response", None, None,
                    Gio.DBusSignalFlags.NONE, lambda c, s, p, i, n, args, _: responses.update({p: args.unpack()}), None)
                def call(method, args):
                    return connection.call_sync(name, path, iface, method, args, GLib.VariantType("(o)"),
                                                Gio.DBusCallFlags.NONE, 10000, None).unpack()[0]
                def request(method, args, timeout=10):
                    handle = call(method, args)
                    until(lambda: handle in responses, timeout)
                    code, result = responses.pop(handle)
                    self.assertEqual(code, 0)
                    return result
                def has_frontend():
                    return connection.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                        "NameHasOwner", GLib.Variant("(s)", (name,)), GLib.VariantType("(b)"), Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
                until(has_frontend)
                # Direct implementation calls must not bypass the public frontend.
                with self.assertRaises(GLib.Error):
                    connection.call_sync("org.freedesktop.impl.portal.desktop.ferese", path, "org.freedesktop.impl.portal.ScreenCast",
                        "CreateSession", GLib.Variant("(oosa{sv})", (path + "/request/test", path + "/session/test", "", {})),
                        None, Gio.DBusCallFlags.NONE, 5000, None)
                def create(token):
                    session = request("CreateSession", GLib.Variant("(a{sv})", ({"session_handle_token": GLib.Variant("s", token)},)))["session_handle"]
                    # An ordinary portal client cannot suppress its stop indicator.
                    with self.assertRaises(GLib.Error) as denied:
                        connection.call_sync("org.freedesktop.impl.portal.desktop.ferese", "/org/ferese/ScreenRecorder",
                            "org.ferese.ScreenRecorder", "UseBarControls", GLib.Variant("(o)", (session,)),
                            None, Gio.DBusCallFlags.NONE, 5000, None)
                    self.assertIn("AccessDenied", str(denied.exception))
                    request("SelectSources", GLib.Variant("(oa{sv})", (session, {"types": GLib.Variant("u", 1)})))
                    return session
                def helpers():
                    return [pid for children in Path(f"/proc/{backend.pid}/task").glob("*/children")
                            for pid in children.read_text().split()]
                session = create("canceltest")
                handle = call("Start", GLib.Variant("(osa{sv})", (session, "", {})))
                until(helpers)
                connection.call_sync(name, handle, "org.freedesktop.portal.Request", "Close", None, None, Gio.DBusCallFlags.NONE, 5000, None)
                until(lambda: not helpers())
                print("PASS: public session negotiation, caller authorization, request cancellation", flush=True)
                if os.environ.get("FERESE_TEST_PORTAL_CONSENT") == "1":
                    session = create("consenttest")
                    print("Select the temporary display and click Share in the nested preview.", flush=True)
                    result = request("Start", GLib.Variant("(osa{sv})", (session, "", {})), 120)
                    reply, fds = connection.call_with_unix_fd_list_sync(name, path, iface, "OpenPipeWireRemote",
                        GLib.Variant("(oa{sv})", (session, {})), GLib.VariantType("(h)"), Gio.DBusCallFlags.NONE, 5000, None, None)
                    fd = fds.get(reply.unpack()[0])
                    try:
                        consume(result["streams"][0][0], fd)
                    finally:
                        os.close(fd)
                    connection.call_sync(name, session, "org.freedesktop.portal.Session", "Close", None, None, Gio.DBusCallFlags.NONE, 5000, None)
                    print("PASS: native consent and video through restricted portal FD", flush=True)
                session = create("frontenddeath")
                call("Start", GLib.Variant("(osa{sv})", (session, "", {})))
                until(helpers)
                frontend.terminate()
                frontend.wait(timeout=5)
                until(lambda: not helpers())
                print("PASS: frontend death revokes pending consent", flush=True)
                sources = json.loads(subprocess.check_output([str(binary), "--sources"], env=env, timeout=10))
                worker = launch("capture", [str(binary), "--stream", sources[0]["name"], "hidden"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
                node = read_node(worker)
                consume(node)
                worker.stdin.close()
                self.assertEqual(worker.wait(timeout=7), 0)
                print("PASS: frame delivery and control-pipe shutdown", flush=True)
                # Test revocation with NO consumer: paused captures must also end on lock.
                worker = launch("paused", [str(binary), "--stream", sources[0]["name"], "hidden"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
                read_node(worker)
                pam = root / "pam"
                pam.mkdir()
                (pam / "ferese-lock").write_text("auth required pam_deny.so\naccount required pam_deny.so\n")
                env["FERESE_LOCK_READY"] = "1"
                locker = launch("locker", ["bwrap", "--bind", "/", "/", "--dev-bind", "/dev", "/dev", "--ro-bind", str(pam), "/etc/pam.d", "--unshare-user", "--", str(repo / "target/release/ferese-lock")])
                self.assertEqual(locker.wait(timeout=20), 0)
                self.assertEqual(worker.wait(timeout=7), 0)
                print("PASS: paused capture revoked by session lock", flush=True)
            except BaseException:
                for log in logs:
                    log.flush()
                    print(Path(log.name).name, Path(log.name).read_text()[-3000:])
                raise
            finally:
                for child in reversed(processes):
                    try:
                        os.killpg(child.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    try:
                        child.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        os.killpg(child.pid, signal.SIGKILL)
                        child.wait()
                    for stream in (child.stdin, child.stdout):
                        if stream is not None:
                            stream.close()
                for log in logs:
                    log.close()


if __name__ == "__main__":
    unittest.main()
