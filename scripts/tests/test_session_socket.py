"""Check IPC discovery for isolated compositor drivers."""
from pathlib import Path
from unittest.mock import patch
import tempfile
import unittest

from session_socket import ipc_environment, ipc_socket


class SessionSocketTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.runtime = Path(self.directory.name)
        # Only path discovery is under test; endpoint permissions and actual
        # socket binding are covered by the compositor's IPC tests.
        self.socket_type = patch.object(Path, 'is_socket', Path.is_file)
        self.socket_type.start()
        self.addCleanup(self.socket_type.stop)

    def endpoint(self, suffix):
        path = self.runtime / suffix
        path.parent.mkdir(parents=True, exist_ok=True)
        path.touch()
        return path

    def test_discovery_changes_when_the_nested_listener_appears(self):
        self.assertEqual(ipc_socket(self.runtime), self.runtime / 'ferese/control.sock')
        nested = self.endpoint('ferese/instances/123-abc/control.sock')
        self.assertEqual(ipc_socket(self.runtime), nested)

    def test_client_environment_replaces_an_inherited_host_target(self):
        nested = self.endpoint('ferese/instances/123-abc/control.sock')
        environment = {'XDG_RUNTIME_DIR': str(self.runtime), 'FERESE_SOCKET': '/run/user/1000/ferese/control.sock'}
        self.assertEqual(ipc_environment(environment)['FERESE_SOCKET'], str(nested))
        self.assertEqual(environment['FERESE_SOCKET'], '/run/user/1000/ferese/control.sock')

    def test_proxy_or_drm_endpoint_remains_supported(self):
        default = self.endpoint('ferese/control.sock')
        self.assertEqual(ipc_socket(self.runtime), default)

    def test_multiple_instances_require_an_explicit_test_target(self):
        self.endpoint('ferese/instances/123-abc/control.sock')
        self.endpoint('ferese/instances/456-def/control.sock')
        with self.assertRaises(RuntimeError):
            ipc_socket(self.runtime)


if __name__ == '__main__':
    unittest.main()
