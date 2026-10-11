"""Package layout and release-input regressions; no root or network needed."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('package', Path(__file__).resolve().parents[1] / 'package.py')
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


class PackageTests(unittest.TestCase):
    def test_release_tag_must_match_workspace_version(self):
        PACKAGE.check_tag('v0.1.0', '0.1.0')
        PACKAGE.check_tag('v0.2.0-rc.1', '0.2.0-rc.1')
        with self.assertRaises(ValueError):
            PACKAGE.check_tag('v0.2.0', '0.1.0')

    def test_refuses_live_root(self):
        with self.assertRaises(ValueError):
            PACKAGE.stage('/', '/missing', 'fedora')

    def test_refuses_existing_content_and_symlink_destinations(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            destination = root / 'dest'
            destination.mkdir()
            (destination / 'keep').write_text('existing')
            link = root / 'link'
            link.symlink_to(destination)
            for path in (destination, link):
                with self.assertRaises(ValueError):
                    PACKAGE.stage(path, root / 'missing', 'fedora')

            self.assertEqual((destination / 'keep').read_text(), 'existing')

    def test_missing_binary_fails_before_staging(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(ValueError):
                PACKAGE.stage(root / 'dest', root / 'missing', 'fedora')

            self.assertFalse((root / 'dest').exists())

    def test_complete_package_layout_and_both_pam_policies(self):
        inventory = json.loads((PACKAGE.ROOT / 'scripts/installer/inventory.json').read_text())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries = root / 'bin'
            binaries.mkdir()
            for entry in inventory['files']:
                if entry['source'].startswith('target/release/'):
                    (binaries / entry['target']).write_bytes(b'fixture')

            for pam in ('fedora', 'arch'):
                with self.subTest(pam=pam), patch.object(PACKAGE, 'licenses'), patch.object(PACKAGE, 'run', return_value='{}'):
                    dest = root / pam
                    PACKAGE.stage(dest, binaries, pam)
                    for entry in inventory['files']:
                        if entry.get('link'):
                            name = Path(entry['source']).name
                            link = dest / 'usr/bin' / name
                            self.assertEqual(link.readlink(), Path('/usr/libexec/ferese') / name)
                            self.assertTrue((dest / str(link.readlink()).lstrip('/')).is_file())

                    for file in dest.rglob('*'):
                        if file.is_file() and not file.is_symlink() and file.suffix in ('.desktop', '.service'):
                            self.assertNotIn('/usr/local/', file.read_text())

                    self.assertFalse((dest / 'usr/local').exists())
                    self.assertIn('system-auth', (dest / 'etc/pam.d/ferese-lock').read_text())
                    wallpaper = dest / 'usr/libexec/ferese/wallpapers'
                    self.assertEqual(wallpaper.readlink(), Path('/usr/share/ferese/wallpapers'))
                    self.assertTrue((dest / 'usr/share/ferese/wallpapers/ferese.png').is_file())
                    self.assertEqual((dest / 'usr/libexec/ferese/ferese-session').stat().st_mode & 0o777, 0o755)
                    self.assertEqual((dest / 'usr/libexec/ferese').stat().st_mode & 0o777, 0o755)

    def test_same_version_from_different_sources_keeps_both_licenses(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            packages = []
            for source in ('registry+first', 'registry+second'):
                folder = root / source.removeprefix('registry+')
                folder.mkdir()
                (folder / 'LICENSE').write_text(source)
                packages.append({'name': 'dpi', 'version': '0.1.2', 'source': source,
                                 'manifest_path': str(folder / 'Cargo.toml'),
                                 'license': 'MIT', 'license_file': None})

            PACKAGE.licenses(root / 'notices', {'packages': packages})
            texts = {path.read_text() for path in (root / 'notices').glob('dpi-*/LICENSE')}
            self.assertEqual(texts, {'registry+first', 'registry+second'})

    def test_dependency_license_report_contains_source_and_license(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'Cargo.toml').write_text('')
            (root / 'LICENSE-MIT').write_text('test license')
            metadata = {'packages': [{'name': 'fixture', 'version': '1.0.0',
                         'source': 'registry+test', 'manifest_path': str(root / 'Cargo.toml'),
                         'license': 'MIT', 'license_file': None}]}
            PACKAGE.licenses(root / 'notices', metadata)
            self.assertEqual(next((root / 'notices').glob('fixture-1.0.0-*/LICENSE-MIT')).read_text(), 'test license')
            report = json.loads((root / 'notices/dependencies.json').read_text())
            self.assertEqual(report[0]['license'], 'MIT')
            self.assertEqual(report[0]['source'], 'registry+test')


if __name__ == '__main__':
    unittest.main()
