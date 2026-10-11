"""Check native portal wire contracts on a private bus without touching the desktop."""
import os
import json
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
import xml.etree.ElementTree as ET

from fixtures.theme_owner import ThemeOwner

REPO = Path(__file__).resolve().parents[2]
BINARY = REPO / os.environ.get("FERESE_TEST_PORTAL_BINARY", "target/debug/xdg-desktop-portal-ferese")
NAME = "org.freedesktop.impl.portal.desktop.ferese"
PATH = "/org/freedesktop/portal/desktop"


class NativePortalContracts(unittest.TestCase):
    def test_private_bus_contracts(self):
        self.run_private("--private")

    def test_standalone_disk_appearance_contracts(self):
        self.run_private("--private-standalone")

    def run_private(self, mode):
        result = subprocess.run(["dbus-run-session", "--", sys.executable, __file__, mode],
                                capture_output=True, text=True, timeout=45)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


def private_bus_checks(standalone=False):
    import gi
    gi.require_version("Gio", "2.0")
    from gi.repository import Gio, GLib

    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(interface, method, parameters, signature=None, destination=NAME):
        return bus.call_sync(destination, PATH, interface, method, parameters,
                             GLib.VariantType.new(signature) if signature else None,
                             Gio.DBusCallFlags.NO_AUTO_START, 5000, None).unpack()

    with tempfile.TemporaryDirectory(prefix="ferese-portal-contracts-") as directory:
        root = Path(directory)
        config = root / "ferese/config.kdl"
        config.parent.mkdir()
        if standalone:
            config.write_text('theme { mode "light"; colors { surface-base "#ffffff"; accent "#ff8000"; }; }; animations { reduced-motion #true; }\n')
        else:
            # A live owner takes priority over this deliberately different document.
            config.write_text('theme { mode "dark"; }; animations { reduced-motion #false; }\n')
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        theme_owner = None if standalone else ThemeOwner(runtime)
        log = (root / "backend.log").open("w")
        env = dict(os.environ, XDG_CONFIG_HOME=directory, XDG_RUNTIME_DIR=str(runtime),
                   WAYLAND_DISPLAY="wayland-unavailable")
        env.pop("WAYLAND_SOCKET", None)
        env.pop("FERESE_SOCKET", None)
        process = subprocess.Popen([str(BINARY)], env=env, stdout=log, stderr=log, start_new_session=True)
        try:
            deadline = time.monotonic() + 15
            while True:
                try:
                    xml, = call("org.freedesktop.DBus.Introspectable", "Introspect", None, "(s)")
                    break
                except GLib.Error:
                    if time.monotonic() > deadline:
                        raise AssertionError((root / "backend.log").read_text())
                    time.sleep(0.05)
            node = ET.fromstring(xml)
            for name in ("ScreenCast", "Settings", "Screenshot", "Wallpaper", "Background", "Usb", "Lockdown", "GlobalShortcuts", "Inhibit", "InputCapture"):
                interface = node.find(f"interface[@name='org.freedesktop.impl.portal.{name}']")
                assert interface is not None, name
                expected = ET.parse(f"/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.{name}.xml").getroot().find("interface")
                for member in expected:
                    if member.tag not in ("method", "signal", "property"):
                        continue
                    actual = interface.find(f"{member.tag}[@name='{member.attrib['name']}']")
                    assert actual is not None, (name, member.tag, member.attrib["name"])
                    if member.tag == "property":
                        assert actual.attrib["type"] == member.attrib["type"]
                        assert actual.attrib["access"] == member.attrib["access"]
                    else:
                        def types(element, direction):
                            return [arg.attrib["type"] for arg in element.findall("arg")
                                    if arg.attrib.get("direction", "out" if element.tag == "signal" else "in") == direction]
                        assert types(actual, "in") == types(member, "in"), (name, member.attrib["name"], "in")
                        assert types(actual, "out") == types(member, "out"), (name, member.attrib["name"], types(actual, "out"), types(member, "out"))
            values, = call("org.freedesktop.impl.portal.Settings", "ReadAll", GLib.Variant("(as)", ([],)), "(a{sa{sv}})")
            appearance = values["org.freedesktop.appearance"]
            assert appearance["color-scheme"] == 2 and appearance["reduced-motion"] == 1, appearance
            if standalone:
                # The resolver adjusts the requested orange for contrast in Light mode.
                red, green, blue = appearance["accent-color"]
                assert 1 >= red > green > blue >= 0, appearance
            else:
                assert appearance["accent-color"][0] == 1. and appearance["accent-color"][2] == 0., appearance
            result, = call("org.freedesktop.impl.portal.Settings", "ReadAll", GLib.Variant("(as)", (["other.*"],)), "(a{sa{sv}})")
            assert result == {}
            try:
                call("org.freedesktop.impl.portal.Settings", "Read", GLib.Variant("(ss)", ("org.freedesktop.appearance", "missing")))
                raise AssertionError("Unknown Settings key succeeded")
            except GLib.Error as error:
                assert "InvalidArgs" in str(error)

            params = GLib.Variant("(ossa{sv})", (PATH + "/request/test/screenshot", "org.test.App", "", {"target": GLib.Variant("u", 99)}))
            try:
                call("org.freedesktop.impl.portal.Screenshot", "Screenshot", params)
                raise AssertionError("Untrusted backend caller succeeded")
            except GLib.Error:
                pass
            shortcut_session = PATH + "/session/test/shortcuts"
            shortcut_handle = PATH + "/request/test/shortcuts"
            create_shortcuts = GLib.Variant("(oosa{sv})", (shortcut_handle, shortcut_session, "org.test.Shortcuts", {}))
            try:
                call("org.freedesktop.impl.portal.GlobalShortcuts", "CreateSession", create_shortcuts)
                raise AssertionError("Untrusted shortcuts creator succeeded")
            except GLib.Error as error:
                assert "AccessDenied" in str(error), error
            original_config = config.read_text()
            try:
                call("org.freedesktop.DBus.Properties", "Set", GLib.Variant("(ssv)", ("org.freedesktop.impl.portal.Lockdown", "disable-printing", GLib.Variant("b", True))))
                raise AssertionError("Untrusted policy writer succeeded")
            except GLib.Error as error:
                assert "AccessDenied" in str(error), error
            printing, = call("org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", ("org.freedesktop.impl.portal.Lockdown", "disable-printing")), "(v)")
            assert printing is False and config.read_text() == original_config
            request_name = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                                         GLib.Variant("(su)", ("org.freedesktop.portal.Desktop", 0)), GLib.VariantType.new("(u)"),
                                         Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]
            assert request_name == 1
            capture = "org.freedesktop.impl.portal.InputCapture"
            caps, = call("org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", (capture, "SupportedCapabilities")), "(v)")
            assert caps == 3
            session = PATH + "/session/test/inputcapture"
            call(capture, "CreateSession2", GLib.Variant("(osa{sv})", (session, "org.test.Capture", {})), "(a{sv})")
            try:
                call(capture, "GetZones", GLib.Variant("(oosa{sv})", (PATH + "/request/test/zones", session, "org.test.Capture", {})))
                raise AssertionError("Zones available without approval")
            except GLib.Error as error:
                assert "InvalidArgs" in str(error), error
            bus.call_sync(NAME, session, "org.freedesktop.impl.portal.Session", "Close", None, None, Gio.DBusCallFlags.NONE, 5000, None)

            version, = call("org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", ("org.freedesktop.impl.portal.ScreenCast", "version")), "(v)")
            assert version == 4
            source_types, = call("org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", ("org.freedesktop.impl.portal.ScreenCast", "AvailableSourceTypes")), "(v)")
            assert source_types == 3
            for mode in (0, 1, 2):
                session = PATH + f"/session/test/persist{mode}"
                handle = PATH + f"/request/test/persist{mode}"
                assert call("org.freedesktop.impl.portal.ScreenCast", "CreateSession", GLib.Variant("(oosa{sv})", (handle, session, "org.test.Share", {})), "(ua{sv})") == (0, {})
                options = {"types": GLib.Variant("u", (1, 2, 3)[mode]), "persist_mode": GLib.Variant("u", mode), "restore_data": GLib.Variant("(suv)", ("Other", 9, GLib.Variant("s", "foreign data")))}
                assert call("org.freedesktop.impl.portal.ScreenCast", "SelectSources", GLib.Variant("(oosa{sv})", (handle, session, "org.test.Share", options)), "(ua{sv})") == (0, {})
                bus.call_sync(NAME, session, "org.freedesktop.impl.portal.Session", "Close", None, None, Gio.DBusCallFlags.NONE, 5000, None)
            session = PATH + "/session/test/badpersist"
            handle = PATH + "/request/test/badpersist"
            call("org.freedesktop.impl.portal.ScreenCast", "CreateSession", GLib.Variant("(oosa{sv})", (handle, session, "org.test.Share", {})))
            try:
                call("org.freedesktop.impl.portal.ScreenCast", "SelectSources", GLib.Variant("(oosa{sv})", (handle, session, "org.test.Share", {"persist_mode": GLib.Variant("u", 3)})))
                raise AssertionError("Invalid persistence mode succeeded")
            except GLib.Error as error:
                assert "InvalidArgs" in str(error), error
            response, values = call("org.freedesktop.impl.portal.Screenshot", "Screenshot", params, "(ua{sv})")
            assert response == 2 and values == {}

            call("org.freedesktop.DBus.Properties", "Set", GLib.Variant("(ssv)", ("org.freedesktop.impl.portal.Lockdown", "disable-printing", GLib.Variant("b", True))))
            printing, = call("org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", ("org.freedesktop.impl.portal.Lockdown", "disable-printing")), "(v)")
            assert printing is True
            assert "disable-printing #true" in config.read_text()
            call("org.freedesktop.DBus.Properties", "Set", GLib.Variant("(ssv)", ("org.freedesktop.impl.portal.Lockdown", "disable-printing", GLib.Variant("b", False))))

            policy_signals = []
            subscription = bus.signal_subscribe(NAME, "org.freedesktop.DBus.Properties", "PropertiesChanged", PATH,
                                                None, Gio.DBusSignalFlags.NONE, lambda *args: policy_signals.append(args[-1].unpack()))
            valid_policy = config.read_text().replace("disable-printing #false", "disable-printing #true")
            config.write_text(valid_policy)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and not any(s[0] == "org.freedesktop.impl.portal.Lockdown" and s[1].get("disable-printing") is True for s in policy_signals):
                GLib.MainContext.default().iteration(False)
                time.sleep(0.02)
            assert any(s[0] == "org.freedesktop.impl.portal.Lockdown" and s[1].get("disable-printing") is True for s in policy_signals), policy_signals
            for invalid_policy in ('portals { lockdown { disable-printing "bad"; }; }', 'portals { lockdown "bad"; }', 'portals #false'):
                config.write_text(original_config + "\n" + invalid_policy)
                time.sleep(1.2)
                printing, = call("org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", ("org.freedesktop.impl.portal.Lockdown", "disable-printing")), "(v)")
                assert printing is True, invalid_policy
            config.write_text(valid_policy)
            bus.signal_unsubscribe(subscription)

            response, values = call("org.freedesktop.impl.portal.GlobalShortcuts", "CreateSession", create_shortcuts, "(ua{sv})")
            assert response == 0 and values == {}
            response, values = call("org.freedesktop.impl.portal.GlobalShortcuts", "ListShortcuts", GLib.Variant("(oo)", (shortcut_handle, shortcut_session)), "(ua{sv})")
            assert response == 0 and values == {"shortcuts": []}, values
            duplicate = [("record", {"description": GLib.Variant("s", "Record screen")})] * 2
            try:
                call("org.freedesktop.impl.portal.GlobalShortcuts", "BindShortcuts", GLib.Variant("(ooa(sa{sv})sa{sv})", (shortcut_handle, shortcut_session, duplicate, "", {})))
                raise AssertionError("Duplicate shortcut IDs accepted")
            except GLib.Error as error:
                assert "InvalidArgs" in str(error), error
            bus.call_sync(NAME, shortcut_session, "org.freedesktop.impl.portal.Session", "Close", None, None,
                          Gio.DBusCallFlags.NO_AUTO_START, 5000, None)
            try:
                call("org.freedesktop.impl.portal.GlobalShortcuts", "ListShortcuts", GLib.Variant("(oo)", (shortcut_handle, shortcut_session)))
                raise AssertionError("Closed shortcut session remains accessible")
            except GLib.Error as error:
                assert "AccessDenied" in str(error), error

            usb_params = GLib.Variant("(oss a(sa{sv}a{sv}) a{sv})".replace(" ", ""),
                (PATH + "/request/test/usb", "", "org.test.App", [], {}))
            response, values = call("org.freedesktop.impl.portal.Usb", "AcquireDevices", usb_params, "(ua{sv})")
            assert response == 2 and values == {}
            call("org.freedesktop.impl.portal.Background", "GetAppState", None, "(a{sv})")

            script = root / "startup.py"
            script.write_text("import json, os, sys\nfd = int(sys.argv[2])\ntry:\n os.fstat(fd); inherited = True\nexcept OSError:\n inherited = False\nopen(sys.argv[1], 'w').write(json.dumps({'args': sys.argv[3:], 'private_fd': inherited, 'socket_env': os.environ.get('WAYLAND_SOCKET')}))\n")
            output = root / "startup.json"
            read_fd, write_fd = os.pipe()
            try:
                arguments = ["value with spaces", "50%", '$dollar `backtick` "quotes" \\ slash']
                argv = [sys.executable, str(script), str(output), str(write_fd)] + arguments
                result, = call("org.freedesktop.impl.portal.Background", "EnableAutostart",
                    GLib.Variant("(sbasu)", ("org.test.Startup", True, argv, 0)), "(b)")
                assert result is True
                os.mkfifo(root / "autostart/000-fifo.desktop")
                env = dict(os.environ, XDG_CONFIG_HOME=directory, XDG_CONFIG_DIRS=str(root / "empty"), XDG_CURRENT_DESKTOP="Ferese", WAYLAND_SOCKET=str(write_fd))
                launcher = REPO / "target/debug/feresectl"
                subprocess.run([str(launcher), "autostart"], env=env, pass_fds=(write_fd,), check=True, timeout=20)
                deadline = time.monotonic() + 5
                while not output.exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                actual = json.loads(output.read_text())
                assert actual == {"args": arguments, "private_fd": False, "socket_env": None}, actual
                result, = call("org.freedesktop.impl.portal.Background", "EnableAutostart",
                    GLib.Variant("(sbasu)", ("org.test.Startup", False, [], 0)), "(b)")
                assert result is True and not (root / "autostart/org.test.Startup.desktop").exists()
            finally:
                os.close(read_fd)
                os.close(write_fd)

            signals = []
            subscription = bus.signal_subscribe(NAME, "org.freedesktop.impl.portal.Settings", "SettingChanged", PATH,
                                                None, Gio.DBusSignalFlags.NONE, lambda *args: signals.append(args[-1].unpack()))
            if standalone:
                temporary = config.with_suffix(".tmp")
                temporary.write_text('theme { mode "dark"; colors { accent "#0080ff"; }; accessibility { increase-contrast #true; }; }; animations { enabled #true; }\n')
                temporary.replace(config)
            else:
                theme_owner.publish("dark", "#0080ff", False, True)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and not {"color-scheme", "accent-color", "reduced-motion", "contrast"}.issubset({signal[1] for signal in signals}):
                GLib.MainContext.default().iteration(False)
                time.sleep(0.02)
            assert any(signal[1] == "color-scheme" and signal[2] == 1 for signal in signals), signals
            changed = {event[1]: event[2] for event in signals}
            if standalone:
                assert changed["accent-color"] != appearance["accent-color"], changed
            else:
                assert changed["accent-color"] == (0., 128 / 255, 1.), changed
            assert changed["reduced-motion"] == 0 and changed["contrast"] == 1, changed
            current, = call("org.freedesktop.impl.portal.Settings", "ReadAll", GLib.Variant("(as)", (["org.freedesktop.*"],)), "(a{sa{sv}})")
            assert current["org.freedesktop.appearance"] == changed, current
            # An invalid edit retains the last valid appearance for either source.
            config.write_text('theme { broken')
            time.sleep(1.2)
            scheme, = call("org.freedesktop.impl.portal.Settings", "Read", GLib.Variant("(ss)", ("org.freedesktop.appearance", "color-scheme")))
            assert scheme == 1, scheme
            bus.signal_unsubscribe(subscription)
        finally:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=5)
            log.close()
            if theme_owner is not None:
                theme_owner.close()


if __name__ == "__main__":
    if sys.argv[1:] == ["--private"]:
        private_bus_checks()
    elif sys.argv[1:] == ["--private-standalone"]:
        private_bus_checks(standalone=True)
    else:
        unittest.main()
