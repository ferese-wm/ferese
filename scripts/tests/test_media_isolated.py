"""Exercise Now Playing on a private bus and nested desktop.

FERESE_TEST_MEDIA=1 FERESE_TEST_BINARY=target/release/ferese \
FERESE_TEST_CTL=target/release/feresectl python3 scripts/tests/test_media_isolated.py
Requires a Wayland host, PyGObject and dbus-run-session.
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
PLAYER = "org.mpris.MediaPlayer2.Player"
ROOT = "org.mpris.MediaPlayer2"
PATH = "/org/mpris/MediaPlayer2"


@unittest.skipUnless(os.environ.get("FERESE_TEST_MEDIA") == "1", "requires a Wayland host")
class Media(unittest.TestCase):
    def test_private_players_and_actions(self):
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
        def __init__(self, name, status="Paused"):
            self.name = ROOT + "." + name
            self.status = status
            self.title = name + " track"
            self.position = 1_000_000
            self.volume = 0.5
            self.calls = []
            self.reads = 0
            self.can_next = True
            self.connection = Gio.DBusConnection.new_for_address_sync(
                os.environ["DBUS_SESSION_BUS_ADDRESS"],
                Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
                None, None)
            properties = [('PlaybackStatus', 's'), ('Metadata', 'a{sv}'), ('Position', 'x'),
                          ('Rate', 'd'), ('Volume', 'd'), ('CanControl', 'b'), ('CanPlay', 'b'),
                          ('CanPause', 'b'), ('CanGoNext', 'b'), ('CanGoPrevious', 'b'), ('CanSeek', 'b')]
            xml = f'<node><interface name="{ROOT}">' + ''.join(
                f'<property name="{key}" type="{kind}" access="read"/>'
                for key, kind in [('Identity', 's'), ('DesktopEntry', 's'), ('CanRaise', 'b')])
            xml += '<method name="Raise"/></interface>' + f'<interface name="{PLAYER}">'
            xml += ''.join(f'<property name="{key}" type="{kind}" access="{ "readwrite" if key == "Volume" else "read" }"/>' for key, kind in properties)
            xml += '''<method name="PlayPause"/><method name="Next"/><method name="Previous"/>
                <method name="SetPosition"><arg type="o" direction="in"/><arg type="x" direction="in"/></method>
                <signal name="Seeked"><arg type="x"/></signal></interface></node>'''
            node = Gio.DBusNodeInfo.new_for_xml(xml)
            self.registrations = [self.connection.register_object(PATH, interface, self.method, self.property, self.set_property)
                                  for interface in node.interfaces]
            self.connection.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                                      "RequestName", GLib.Variant("(su)", (self.name, 0)), None,
                                      Gio.DBusCallFlags.NONE, 2000, None)

        def property(self, connection, sender, path, interface, name):
            self.reads += 1
            if name == 'Metadata':
                return GLib.Variant('a{sv}', {'xesam:title': GLib.Variant('s', self.title),
                    'xesam:artist': GLib.Variant('as', ['Artist']), 'mpris:length': GLib.Variant('x', 120_000_000),
                    'mpris:trackid': GLib.Variant('o', '/track/one')})
            if name == 'PlaybackStatus':
                return GLib.Variant('s', self.status)
            if name in ('Identity', 'DesktopEntry'):
                return GLib.Variant('s', self.name.rsplit('.', 1)[-1])
            if name == 'Position':
                return GLib.Variant('x', self.position)
            if name in ('Rate', 'Volume'):
                return GLib.Variant('d', 1.0 if name == 'Rate' else self.volume)
            return GLib.Variant('b', self.can_next if name == 'CanGoNext' else True)

        def set_property(self, connection, sender, path, interface, name, value):
            if interface == PLAYER and name == 'Volume':
                self.volume = value.unpack()
                self.changed(['Volume'])
                return True
            return False

        def method(self, connection, sender, path, interface, name, parameters, invocation):
            self.calls.append((name, parameters.unpack()))
            if name == 'PlayPause':
                self.set('Paused' if self.status == 'Playing' else 'Playing')
            elif name == 'SetPosition':
                self.position = parameters.unpack()[1]
                self.connection.emit_signal(None, PATH, PLAYER, 'Seeked', GLib.Variant('(x)', (self.position,)))
            invocation.return_value(GLib.Variant('()', ()))

        def changed(self, fields):
            self.connection.emit_signal(None, PATH, 'org.freedesktop.DBus.Properties', 'PropertiesChanged',
                GLib.Variant('(sa{sv}as)', (PLAYER, {}, fields)))
            self.connection.flush_sync(None)

        def set(self, status):
            self.status = status
            self.changed(['PlaybackStatus'])

        def close(self):
            self.connection.close_sync(None)

    players = [FakePlayer('first')]
    child = None
    shell = None
    try:
        with tempfile.TemporaryDirectory(prefix='ferese-media-') as directory:
            root = Path(directory)
            runtime = root / 'runtime'
            runtime.mkdir(mode=0o700)
            config = root / 'config/ferese'
            config.mkdir(parents=True)
            (config / 'config.kdl').write_text('animations { reduced-motion #true; }\n')
            display = Path(os.environ['WAYLAND_DISPLAY'])
            if not display.is_absolute():
                display = Path(os.environ['XDG_RUNTIME_DIR']) / display
            env = dict(os.environ, WAYLAND_DISPLAY=str(display), XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / 'config'), XDG_STATE_HOME=str(root / 'state'))
            for variable in ['WAYLAND_SOCKET', 'FERESE_SOCKET', 'FERESE_SHELL_CONTROL_SOCKET']:
                env.pop(variable, None)
            binary = REPO / os.environ.get('FERESE_TEST_BINARY', 'target/debug/ferese')
            ctl = REPO / os.environ.get('FERESE_TEST_CTL', 'target/debug/feresectl')
            with (root / 'compositor.log').open('w') as log:
                child = subprocess.Popen([str(binary), '--backend=nested'], env=ipc_environment(env),
                                         stdout=log, stderr=log, start_new_session=True)

                def wait(check):
                    deadline = time.monotonic() + 8
                    while time.monotonic() < deadline:
                        if child.poll() is not None:
                            raise AssertionError((root / 'compositor.log').read_text())
                        if check():
                            return
                        time.sleep(.02)
                    raise AssertionError('Timed out\n' + (root / 'compositor.log').read_text())

                wait(lambda: (ipc_socket(runtime)).exists())

                def call(*args):
                    return json.loads(subprocess.check_output([str(ctl), "-j", *args], env=ipc_environment(env), timeout=5))

                def request(command, args):
                    payload = json.dumps({'version': 1, 'id': 17, 'type': 'command',
                                          'command': command, 'args': args}).encode()
                    connection = socket.socket(socket.AF_UNIX)
                    connection.settimeout(3)
                    connection.connect(str(ipc_socket(runtime)))
                    connection.sendall(struct.pack('!I', len(payload)) + payload)
                    return connection

                def response(connection):
                    def read(size):
                        data = b''
                        while len(data) < size:
                            chunk = connection.recv(size - len(data))
                            assert chunk, "IPC closed before the response completed"
                            data += chunk
                        return data
                    result = json.loads(read(struct.unpack('!I', read(4))[0]))
                    connection.close()
                    assert result['id'] == 17
                    return result

                def selected(name):
                    wait(lambda: call('media')['selected'] and call('media')['selected']['name'] == name)

                env['WAYLAND_DISPLAY'] = str(next(path for path in runtime.glob('wayland-*') if not path.name.endswith('.lock')))
                shell_binary = os.environ.get('FERESE_TEST_SHELL')
                if shell_binary:
                    shell = subprocess.Popen([str(REPO / shell_binary)], env=ipc_environment(env), stdout=log, stderr=log, start_new_session=True)
                first = players[0]
                selected(first.name)
                initial = call('media')
                watcher = request('media-watch', {'since': initial['revision']})
                first.set('Playing')
                update = response(watcher)['result']
                assert update['selected']['status'] == 'playing'
                assert update['selected']['title'] == 'first track'
                assert update['selected']['artist'] == 'Artist'
                # No timer queries while the player is idle and unchanged.
                time.sleep(.1)
                reads = first.reads
                time.sleep(1.2)
                assert first.reads == reads, (first.reads, reads)
                second = FakePlayer('second', 'Playing')
                players.append(second)
                selected(second.name)
                first.title = 'A different first track'
                first.changed(['Metadata'])
                time.sleep(.1)
                assert call('media')['selected']['name'] == second.name
                call('media', 'pin', first.name)
                selected(first.name)
                call('media', 'next')
                assert first.calls[-1][0] == 'Next'
                call('media', 'previous')
                assert first.calls[-1][0] == 'Previous'
                call('media', 'raise')
                assert first.calls[-1][0] == 'Raise'
                assert 'error' not in response(request('media-action', {'action': 'volume', 'delta': 0.1}))
                wait(lambda: abs(call('media')['selected']['volume'] - 0.6) < 1e-9)
                assert abs(first.volume - 0.6) < 1e-9
                assert 'error' in response(request('media-action', {'action': 'volume', 'delta': 2.0}))
                stale = response(request('media-action', {'action': 'play-pause', 'player': first.name, 'owner': ':999999'}))
                assert 'error' in stale
                before = len(first.calls)
                first.can_next = False
                first.changed(['CanGoNext'])
                wait(lambda: not call('media')['selected']['can_next'])
                assert 'error' in response(request('media-action', {'action': 'next'}))
                assert len(first.calls) == before
                assert 'error' in response(request('media-action', {'action': 'seek', 'track_id': '/track/stale', 'position_us': 10_000_000}))
                assert 'error' not in response(request('media-action', {'action': 'seek', 'track_id': '/track/one', 'position_us': 10_000_000}))
                wait(lambda: call('media')['selected']['position_us'] == 10_000_000)
                call('media', 'ignore', first.name)
                selected(second.name)
                call('media', 'unignore', first.name)
                call('media', 'pin', first.name)
                first.close()
                players.remove(first)
                selected(second.name)
                assert call('media')['pinned'] is None
                replacement = FakePlayer('first', 'Paused')
                players.append(replacement)
                wait(lambda: len(call('media')['players']) == 2)
                assert call('media')['selected']['name'] == second.name
                call('media', 'play-pause')
                wait(lambda: second.status == 'Paused')
                second.set('Stopped')
                selected(replacement.name)
                replacement.set('Stopped')
                wait(lambda: call('media')['selected'] is None)
                assert child.poll() is None
                if shell:
                    time.sleep(.3)
                    assert shell.poll() is None, (root / "compositor.log").read_text()
                print('MPRIS discovery, selection, watches, capabilities, stale actions, seeking and owner replacement passed.')
    finally:
        for player in players:
            player.close()
        for process in [shell, child]:
            if process and process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
        loop.quit()


if __name__ == '__main__':
    if sys.argv[1:] == ['--private']:
        private_checks()
    else:
        unittest.main()
