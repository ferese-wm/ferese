"""Capability descriptor ownership in the packaged wrapper, using inert helpers."""
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / 'packaging/ferese-session-shell'
PROBE = '''#!/usr/bin/python3
import json, os, pathlib, sys, time
root = pathlib.Path(os.environ['TEST_ROOT'])
name = pathlib.Path(sys.argv[0]).name
fds = [int(value) for value in os.environ['TEST_FDS'].split(',')]
held = []
for fd in fds:
    try:
        held.append(os.readlink(f'/proc/self/fd/{fd}'))
    except OSError:
        pass
(root / (name + '.json')).write_text(json.dumps({
    'held': held,
    'private_env': [key for key in ['WAYLAND_SOCKET', 'FERESE_SHELL_CONTROL_SOCKET'] if key in os.environ],
}))
if name in ['ferese-shell', 'ferese-polkit-agent']:
    for fd in fds:
        try:
            os.close(fd)
        except OSError:
            pass
    (root / (name + '.ready')).touch()
    while True:
        time.sleep(.02)
'''


class SessionDescriptorTest(unittest.TestCase):
    def test_only_shell_inherits_private_sockets_and_wrapper_releases_them(self):
        for managed in (False, True):
            with self.subTest(managed=managed), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                shutil.copyfile(SCRIPT, root / SCRIPT.name)
                helpers = ['ferese-shell', 'ferese-polkit-agent', 'feresectl',
                           'systemctl', 'dbus-send', 'dbus-update-activation-environment']
                for name in helpers:
                    path = root / name
                    path.write_text(PROBE)
                    path.chmod(0o755)
                pairs = [socket.socketpair(), socket.socketpair()]
                fds = [child.fileno() for _, child in pairs]
                expected = [os.readlink(f'/proc/self/fd/{fd}') for fd in fds]
                env = dict(os.environ, PATH=f'{root}:{os.environ["PATH"]}',
                           TEST_ROOT=directory, TEST_FDS=','.join(map(str, fds)),
                           WAYLAND_SOCKET=str(fds[0]), FERESE_SHELL_CONTROL_SOCKET=str(fds[1]),
                           FERESE_PUBLIC_WAYLAND_DISPLAY='test-public',
                           FERESE_SESSION_MODE='desktop', FERESE_SESSION_IMPORT_ENV=str(int(managed)),
                           FERESE_CLIENT_RESTART='0')
                child = subprocess.Popen(['bash', str(root / SCRIPT.name)], env=env,
                                         pass_fds=fds, start_new_session=True,
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                for _, peer in pairs:
                    peer.close()
                try:
                    required = ['ferese-shell', 'ferese-polkit-agent']
                    if managed:
                        required = helpers
                    deadline = time.monotonic() + 5
                    while not (all((root / (name + '.json')).exists() for name in required)
                               and (root / 'ferese-shell.ready').exists()
                               and (root / 'ferese-polkit-agent.ready').exists()):
                        self.assertLess(time.monotonic(), deadline, 'helper startup timed out')
                        time.sleep(.02)
                    for name in required:
                        result = json.loads((root / (name + '.json')).read_text())
                        self.assertEqual(result['held'], expected if name == 'ferese-shell' else [], name)
                        self.assertEqual(result['private_env'],
                                         ['WAYLAND_SOCKET', 'FERESE_SHELL_CONTROL_SOCKET']
                                         if name == 'ferese-shell' else [], name)
                    # Both children closed their copies but remain alive. The
                    # wrapper and agent supervisor must not retain either socket.
                    for server, _ in pairs:
                        server.settimeout(1)
                        self.assertEqual(server.recv(1), b'')
                finally:
                    os.killpg(child.pid, signal.SIGTERM)
                    try:
                        child.communicate(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(child.pid, signal.SIGKILL)
                        child.communicate()
                    for server, _ in pairs:
                        server.close()
