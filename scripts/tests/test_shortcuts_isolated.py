"""Exercise shortcut connection lifetimes in an isolated nested compositor."""
import json
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import signal
import socket
import struct
import subprocess
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[2]


@unittest.skipUnless(os.environ.get("FERESE_TEST_SHORTCUTS") == "1", "requires a Wayland host and a built compositor")
class ShortcutConnections(unittest.TestCase):
    def test_disconnect_conflicts_and_configuration_reload(self):
        host_runtime = Path(os.environ["XDG_RUNTIME_DIR"])
        host_display = Path(os.environ["WAYLAND_DISPLAY"])
        if not host_display.is_absolute():
            host_display = host_runtime / host_display
        with tempfile.TemporaryDirectory(prefix="ferese-shortcut-test-") as directory:
            root = Path(directory)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese/config.kdl"
            config.parent.mkdir(parents=True)
            config.write_text("")
            log = (root / "compositor.log").open("w")
            env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(root / "config"), WAYLAND_DISPLAY=str(host_display))
            env.pop("WAYLAND_SOCKET", None)
            env.pop("FERESE_SHELL_CONTROL_SOCKET", None)
            process = subprocess.Popen([str(REPO / "target/debug/ferese"), "--backend=nested"], env=ipc_environment(env),
                                       stdout=log, stderr=log, start_new_session=True)
            connections = []
            try:
                deadline = time.monotonic() + 15
                while not (ipc_socket(runtime)).exists():
                    if process.poll() is not None or time.monotonic() >= deadline:
                        self.fail((root / "compositor.log").read_text())
                    time.sleep(0.02)

                def connect():
                    connection = socket.socket(socket.AF_UNIX)
                    connection.settimeout(2)
                    connection.connect(str(ipc_socket(runtime)))
                    connections.append(connection)
                    return connection

                def call(connection, command, args=None):
                    data = json.dumps({"version": 1, "id": 1, "type": "command", "command": command, "args": args}).encode()
                    connection.sendall(struct.pack(">I", len(data)) + data)

                    def receive(length):
                        data = b""
                        while len(data) < length:
                            chunk = connection.recv(length - len(data))
                            if not chunk:
                                raise AssertionError("IPC connection closed")
                            data += chunk
                        return data

                    length, = struct.unpack(">I", receive(4))
                    self.assertLessEqual(length, 1024 * 1024)
                    return json.loads(receive(length))

                first = connect()
                second = connect()
                shortcut = [{"id": "record", "trigger": "Ctrl+Alt+a"}]
                self.assertNotIn("error", call(first, "portal-shortcuts-register", shortcut))
                conflict = call(second, "portal-shortcuts-register", shortcut)
                self.assertEqual(conflict["error"]["code"], "shortcut_conflict")
                self.assertTrue(call(second, "portal-shortcuts-poll")["result"]["closed"])
                first.close()
                deadline = time.monotonic() + 3
                while "error" in call(second, "portal-shortcuts-register", shortcut):
                    self.assertLess(time.monotonic(), deadline, "disconnected owner kept its binding")
                    time.sleep(0.02)
                self.assertEqual(call(second, "portal-shortcuts-poll")["result"]["shortcuts"], shortcut)
                config.write_text('binding keys="Ctrl+Alt+a" action="none"\n')
                deadline = time.monotonic() + 5
                while call(second, "portal-shortcuts-poll")["result"]["shortcuts"]:
                    self.assertLess(time.monotonic(), deadline, "configuration did not revoke conflicting shortcut")
                    time.sleep(0.05)
            finally:
                for connection in connections:
                    connection.close()
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=5)
                log.close()


if __name__ == "__main__":
    unittest.main()
