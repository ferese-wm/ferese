"""Run complete installer commands against disposable destination filesystems."""
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

REPO = Path(__file__).resolve().parents[2]
HELPER = REPO / 'scripts/installer/install.py'
spec = importlib.util.spec_from_file_location('ferese_install', HELPER)
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)
PREFIX = installer.PREFIX.lstrip('/')
PORTAL = 'usr/share/xdg-desktop-portal/ferese-portals.conf'
UNIT = 'usr/local/lib/systemd/user/xdg-desktop-portal-ferese.service'
DESKTOP = 'usr/share/wayland-sessions/ferese.desktop'


class InstallerTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.shared = tempfile.TemporaryDirectory()
        cls.source = Path(cls.shared.name) / 'source'
        for rule in installer.INVENTORY['files']:
            if rule['source'].startswith('target/release/'):
                paths = [REPO / rule['source']]
            else:
                paths = sorted(REPO.glob(rule['source']))

            for path in paths:
                target = cls.source / path.relative_to(REPO)
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(b'#!/bin/sh\nexit 0\n' if rule['source'].startswith('target/release/') else path.read_bytes())
                target.chmod(0o755 if rule['source'].startswith('target/release/') else path.stat().st_mode & 0o777)

        cls.bundles = []
        for name in ('first', 'second'):
            for relative in ('portal/ferese-portals.conf', 'systemd/xdg-desktop-portal-ferese.service', 'ferese.desktop'):
                with (cls.source / 'packaging' / relative).open('a') as stream:
                    stream.write(f'\n# {name}\n')

            output = Path(cls.shared.name) / name
            subprocess.run([sys.executable, str(HELPER), 'bundle', '--source-root', str(cls.source),
                            '--output', str(output), '--release-id', name], check=True, capture_output=True)
            cls.bundles.append(output)

    @classmethod
    def tearDownClass(cls):
        cls.shared.cleanup()

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / 'root'
        pam = self.root / 'etc/pam.d/login'
        pam.parent.mkdir(parents=True)
        pam.write_text('test PAM stack\n')
        self.base = self.root / PREFIX

    def run_command(self, *args, success=True, fault=None, fault_after=4):
        command = [sys.executable, str(HELPER)]
        if fault:
            # Inject failure at the filesystem boundary, without shipping production test hooks.
            code = f'''import importlib.util, os, sys
spec = importlib.util.spec_from_file_location('installer', {str(HELPER)!r})
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
original = m.Installation.write_state
count = 0
def write(self, *args, **kwargs):
    global count
    original(self, *args, **kwargs)
    count += 1
    if count == {fault_after}:
        {fault}
m.Installation.write_state = write
m.main()
'''
            command = [sys.executable, '-c', code]

        result = subprocess.run([*command, *map(str, args), '--root', str(self.root)], capture_output=True, text=True)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)

        return result

    def install(self, index=0, **kwargs):
        return self.run_command('install', self.bundles[index], **kwargs)

    def snapshot(self):
        result = {}
        for path in self.root.rglob('*'):
            if path.is_relative_to(self.base / 'transactions') or path.is_relative_to(self.base / 'releases'):
                continue
            if path.is_symlink():
                result[str(path.relative_to(self.root))] = os.readlink(path)
            elif path.is_file() and path.name != '.install.lock':
                result[str(path.relative_to(self.root))] = path.read_bytes()

        return result

    def test_launcher_permissions_are_assigned_when_bundling(self):
        self.assertEqual((self.source / 'packaging/ferese-session').stat().st_mode & 0o111, 0)
        for name in ('ferese-session', 'ferese-session-shell', 'ferese-screenshot'):
            self.assertEqual((self.bundles[0] / name).stat().st_mode & 0o777, 0o755)

    def test_fresh_upgrade_and_rollback_restore_all_integration(self):
        self.install()
        first = [(self.root / path).read_bytes() for path in (PORTAL, UNIT, DESKTOP)]
        self.assertEqual(os.readlink(self.base / 'current'), 'releases/first')
        self.assertEqual(os.readlink(self.root / 'usr/local/bin/ferese'), '/usr/local/lib/ferese/current/ferese')
        self.install(1)
        self.assertEqual(os.readlink(self.base / 'previous'), 'releases/first')
        for path, old in zip((PORTAL, UNIT, DESKTOP), first):
            self.assertNotEqual((self.root / path).read_bytes(), old)

        self.run_command('rollback')
        self.assertEqual(os.readlink(self.base / 'current'), 'releases/first')
        self.assertEqual(os.readlink(self.base / 'previous'), 'releases/second')
        self.assertEqual([(self.root / path).read_bytes() for path in (PORTAL, UNIT, DESKTOP)], first)

    def test_upgrade_and_rollback_accept_retired_wallpaper_svg_with_verified_checksum(self):
        legacy = Path(self.temporary.name) / 'legacy'
        shutil.copytree(self.bundles[0], legacy)
        svg = legacy / 'wallpapers/ferese.svg'
        svg.write_text('<svg xmlns="http://www.w3.org/2000/svg"/>\n')
        svg.chmod(0o644)
        manifest_path = legacy / 'manifest.json'
        manifest = json.loads(manifest_path.read_text())
        manifest['files'].append({'target': 'wallpapers/ferese.svg', 'mode': '0644',
                                  'sha256': installer.digest(svg)})
        manifest_path.write_text(json.dumps(manifest))

        self.run_command('install', legacy)
        self.assertTrue((self.base / 'current/wallpapers/ferese.svg').is_file())
        self.install(1)
        self.assertEqual(os.readlink(self.base / 'current'), 'releases/second')
        self.assertFalse((self.base / 'current/wallpapers/ferese.svg').exists())
        self.run_command('rollback')
        self.assertEqual(os.readlink(self.base / 'current'), 'releases/first')
        self.assertEqual((self.base / 'current/wallpapers/ferese.svg').read_bytes(), svg.read_bytes())

        svg.write_text('corrupt legacy wallpaper')
        with self.assertRaisesRegex(ValueError, 'Checksum mismatch: wallpapers/ferese.svg'):
            installer.validate_bundle(legacy)

    def test_upgrade_uses_previous_installer_ownership_receipts(self):
        self.install()
        release = self.base / 'releases/first'
        (release / 'manifest.json').unlink()
        for rule in installer.INVENTORY['files']:
            if 'destination' in rule and ('portal/' in rule['source'] or 'systemd/' in rule['source']):
                receipt = release / 'installer-files' / rule['source'].removeprefix('packaging/')
                receipt.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(self.root / rule['destination'].lstrip('/'), receipt)

        # The old installer saved no desktop receipt: only identical incoming files are safe.
        (self.root / DESKTOP).write_bytes((self.bundles[1] / ('integration/' + DESKTOP)).read_bytes())
        self.install(1)
        result = self.run_command('rollback', success=False)
        self.assertIn('predates verified bundles', result.stderr)
        self.assertEqual(os.readlink(self.base / 'current'), 'releases/second')

    def test_reinstall_is_idempotent_and_keeps_previous(self):
        self.install()
        self.install(1)
        before = self.snapshot()
        self.install(1)
        self.assertEqual(before, self.snapshot())
        self.assertEqual(len(list((self.base / 'transactions').iterdir())), 2)

    def test_modified_configuration_is_preserved_on_upgrade_and_rollback(self):
        self.install()
        self.install(1)
        (self.root / UNIT).write_text('administrator override\n')
        before = self.snapshot()
        self.run_command('rollback', success=False)
        self.assertEqual(before, self.snapshot())
        self.install(success=False)
        self.assertEqual(before, self.snapshot())

    def test_explicit_portal_replacement_keeps_backup(self):
        self.install()
        (self.root / PORTAL).write_text('administrator preferences\n')
        self.install(1, success=False)
        self.run_command('install', self.bundles[1], '--replace-portal-config')
        backups = list((self.base / 'transactions').glob('*/backup/*'))
        self.assertTrue(any(path.read_text() == 'administrator preferences\n' for path in backups))

    def test_normal_failure_restores_previous_files_and_can_retry(self):
        self.install()
        before = self.snapshot()
        self.install(1, success=False, fault="raise OSError('injected write failure')")
        self.assertEqual(before, self.snapshot())
        self.assertFalse((self.base / '.transaction').exists())
        self.install(1)

    def test_killed_upgrade_recovers_before_next_install(self):
        self.install()
        self.install(1, success=False, fault='os._exit(91)')
        self.assertTrue((self.base / '.transaction/journal.json').is_file())
        result = self.install(1)
        self.assertIn('Recovered interrupted', result.stdout)
        self.assertFalse((self.base / '.transaction').exists())
        self.assertEqual(os.readlink(self.base / 'current'), 'releases/second')

    def test_failure_after_switching_current_restores_previous_release(self):
        self.install()
        before = self.snapshot()
        self.install(1, success=False, fault="raise OSError('failure after current switch')", fault_after=5)
        self.assertEqual(before, self.snapshot())

    def test_killed_install_after_current_switch_is_recovered(self):
        self.install()
        before = self.snapshot()
        self.install(1, success=False, fault='os._exit(91)', fault_after=5)
        self.assertEqual(os.readlink(self.base / 'current'), 'releases/second')
        self.run_command('recover')
        self.assertEqual(before, self.snapshot())

    def test_killed_fresh_install_recovers_absent_files(self):
        before = self.snapshot()
        self.install(success=False, fault='os._exit(91)')
        self.run_command('recover')
        self.assertEqual(before, self.snapshot())

    def test_recovery_preserves_edits_made_after_interruption(self):
        self.install()
        self.install(1, success=False, fault='os._exit(91)')
        (self.root / UNIT).write_text('edited after crash\n')
        result = self.run_command('recover', success=False)
        self.assertIn('Recovery conflict', result.stderr)
        self.assertEqual((self.root / UNIT).read_text(), 'edited after crash\n')
        self.assertTrue((self.base / '.transaction').exists())

    def test_concurrent_install_is_rejected(self):
        self.base.mkdir(parents=True)
        with (self.base / '.install.lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            result = self.install(success=False)

        self.assertIn('in progress', result.stderr)
        self.assertFalse((self.base / 'current').exists())

    def test_restrictive_umask_does_not_make_installed_release_private(self):
        result = subprocess.run([sys.executable, '-c',
                                 f"import os, runpy; os.umask(0o077); runpy.run_path({str(HELPER)!r}, run_name='__main__')",
                                 'install', str(self.bundles[0]), '--root', str(self.root)],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        for relative in ('usr', 'usr/local', PREFIX, PREFIX + '/releases', PREFIX + '/releases/first',
                         PREFIX + '/releases/first/wallpapers', 'usr/share/wayland-sessions'):
            self.assertEqual((self.root / relative).stat().st_mode & 0o777, 0o755, relative)

    def test_dry_run_does_not_write(self):
        before = self.snapshot()
        self.run_command('install', self.bundles[0], '--dry-run')
        self.assertEqual(before, self.snapshot())
        self.assertFalse(self.base.exists())

    def test_killed_directory_creation_can_retry_with_public_permissions(self):
        # Cover persistent parents, including creation before the lock/journal and
        # during publication. Staging trees are unpublished and rebuilt on retry.
        directories = {'usr', 'usr/local', 'usr/local/lib', PREFIX, PREFIX + '/releases',
                       PREFIX + '/transactions', 'usr/local/bin'}
        for rule in installer.INVENTORY['files']:
            if 'destination' in rule:
                parent = Path(rule['destination'].lstrip('/')).parent
                directories.update(str(path) for path in (parent, *parent.parents) if str(path) != '.')
        for relative in sorted(directories):
            with self.subTest(directory=relative), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary) / 'root'
                pam = root / 'etc/pam.d/login'
                pam.parent.mkdir(parents=True)
                pam.write_text('test PAM stack\n')
                target = root / relative
                code = f'''import importlib.util, os
from pathlib import Path
spec = importlib.util.spec_from_file_location('installer', {str(HELPER)!r})
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
original = Path.mkdir
def mkdir(self, *args, **kwargs):
    original(self, *args, **kwargs)
    if self == Path({str(target)!r}):
        os._exit(91)
Path.mkdir = mkdir
os.umask(0o077)
m.main()
'''
                args = ['install', str(self.bundles[0]), '--root', str(root)]
                result = subprocess.run([sys.executable, '-c', code, *args], capture_output=True, text=True)
                self.assertEqual(result.returncode, 91, result.stderr)
                mode = 0o700 if relative.endswith('/transactions') else 0o755
                created_mode = target.stat().st_mode & 0o777
                retry = f"import os, runpy; os.umask(0o077); runpy.run_path({str(HELPER)!r}, run_name='__main__')"
                result = subprocess.run([sys.executable, '-c', retry, *args], capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(created_mode, mode)
                self.assertEqual(target.stat().st_mode & 0o777, mode)
                self.assertEqual(os.readlink(root / PREFIX / 'current'), 'releases/first')
                self.assertFalse((root / PREFIX / '.transaction').exists())

    def test_existing_directory_permissions_are_preserved(self):
        for relative in ('usr', 'usr/local', 'usr/local/lib', PREFIX,
                         PREFIX + '/releases', 'usr/share/wayland-sessions'):
            path = self.root / relative
            path.mkdir(parents=True, exist_ok=True)
            path.chmod(0o750)
        self.install()
        for relative in ('usr', 'usr/local', 'usr/local/lib', PREFIX,
                         PREFIX + '/releases', 'usr/share/wayland-sessions'):
            self.assertEqual((self.root / relative).stat().st_mode & 0o777, 0o750)

    def test_directory_creation_restores_umask_on_success_and_failure(self):
        previous = os.umask(0o077)
        try:
            installer.ensure_directory(self.root / 'new')
            self.assertEqual(os.umask(0o077), 0o077)
            with mock.patch.object(Path, 'mkdir', side_effect=OSError('mkdir failed')):
                with self.assertRaises(OSError):
                    installer.ensure_directory(self.root / 'failed')
            self.assertEqual(os.umask(0o077), 0o077)
        finally:
            os.umask(previous)

    def test_custom_pam_is_preserved(self):
        pam = self.root / 'etc/pam.d/ferese-lock'
        pam.write_text('administrator authentication\n')
        self.install()
        self.install(1)
        self.run_command('rollback')
        self.assertEqual(pam.read_text(), 'administrator authentication\n')

    def test_missing_pam_stack_fails_before_publication(self):
        (self.root / 'etc/pam.d/login').unlink()
        self.install(success=False)
        self.assertFalse((self.base / 'current').exists())
        self.assertFalse((self.root / UNIT).exists())

    def test_symlinked_destination_parent_is_rejected(self):
        outside = Path(self.temporary.name) / 'outside'
        outside.mkdir()
        (self.root / 'usr').symlink_to(outside)
        self.install(success=False)
        self.assertEqual(list(outside.iterdir()), [])

    def test_uninstall_preserves_releases_pam_and_unrelated_files(self):
        self.install()
        unrelated = self.root / 'usr/local/bin/unrelated'
        unrelated.write_text('keep\n')
        self.run_command('uninstall')
        self.assertFalse(os.path.lexists(self.base / 'current'))
        self.assertFalse(os.path.lexists(self.root / 'usr/local/bin/ferese'))
        self.assertFalse((self.root / PORTAL).exists())
        self.assertTrue((self.base / 'releases/first/ferese').is_file())
        self.assertTrue((self.root / 'etc/pam.d/ferese-lock').is_file())
        self.assertEqual(unrelated.read_text(), 'keep\n')

    def test_corrupt_incomplete_unlisted_or_symlink_bundle_is_rejected(self):
        for mutation in ('checksum', 'missing', 'extra', 'symlink', 'mode', 'traversal'):
            with self.subTest(mutation=mutation):
                copy = Path(self.temporary.name) / mutation
                shutil.copytree(self.bundles[0], copy)
                binary = copy / 'ferese'
                if mutation == 'checksum':
                    binary.write_text('corrupt')
                elif mutation == 'missing':
                    binary.unlink()
                elif mutation == 'extra':
                    (copy / 'unexpected').write_text('extra')
                elif mutation == 'symlink':
                    binary.unlink()
                    binary.symlink_to('/etc/passwd')
                elif mutation == 'mode':
                    binary.chmod(0o777)
                else:
                    manifest = json.loads((copy / 'manifest.json').read_text())
                    manifest['files'][0]['target'] = '../../etc/passwd'
                    (copy / 'manifest.json').write_text(json.dumps(manifest))

                self.run_command('install', copy, success=False)
                self.assertFalse((self.base / 'current').exists())

    def test_release_id_collision_does_not_change_current(self):
        self.install()
        changed = Path(self.temporary.name) / 'changed'
        shutil.copytree(self.bundles[0], changed)
        manifest = json.loads((changed / 'manifest.json').read_text())
        manifest['features'] = 'different-build'
        (changed / 'manifest.json').write_text(json.dumps(manifest))
        before = self.snapshot()
        self.run_command('install', changed, success=False)
        self.assertEqual(before, self.snapshot())


class FrontendTest(unittest.TestCase):
    def test_elevation_preserves_arguments_for_sudo_and_pkexec(self):
        import shlex
        for elevation in ('sudo', 'pkexec'):
            with self.subTest(elevation=elevation), tempfile.TemporaryDirectory() as directory:
                bin_dir = Path(directory)
                for tool in ('python3', 'dirname', 'uname', 'date'):
                    (bin_dir / tool).symlink_to(shutil.which(tool))
                elevate = bin_dir / elevation
                elevate.write_text('#!/bin/sh\nexit 99\n')
                elevate.chmod(0o755)
                result = subprocess.run(['/bin/bash', str(REPO / 'scripts/install.sh'), '--bundle',
                                         '/tmp/bundle with spaces', '--replace-portal-config', '--dry-run'],
                                        env=dict(os.environ, PATH=str(bin_dir)), capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                command = shlex.split(result.stdout.removeprefix('Install: '))
                # Building is prohibited as root, but installing an existing bundle is allowed.
                expected = ([] if os.geteuid() == 0 else [elevation]) + [
                    '/usr/bin/bash', str(REPO / 'scripts/install-session.sh'), 'install',
                    '/tmp/bundle with spaces', '--replace-portal-config']
                self.assertEqual(command, expected)

    def test_incompatible_or_missing_options_are_rejected(self):
        for options in (['--bundle'], ['--release-id'], ['--bundle-only'],
                        ['--bundle', '/tmp/bundle', '--offline'],
                        ['--bundle', '/tmp/bundle', '--resize-metrics'],
                        ['--bundle', '/tmp/bundle', '--release-id', 'another'],
                        ['--skip-build']):
            with self.subTest(options=options):
                result = subprocess.run(['/bin/bash', str(REPO / 'scripts/install.sh'), *options],
                                        capture_output=True)
                self.assertNotEqual(result.returncode, 0)


class UserSetupTest(unittest.TestCase):
    def test_repeatable_migration_and_reload_after_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            override = home / '.config/systemd/user/xdg-desktop-portal-ferese.service.d/override.conf'
            override.parent.mkdir(parents=True)
            override.write_text(f'[Service]\nExecStart=\nExecStart={home}/.local/libexec/ferese/xdg-desktop-portal-ferese\n')
            bin_dir = home / 'bin'
            bin_dir.mkdir()
            systemctl = bin_dir / 'systemctl'
            systemctl.write_text('#!/bin/sh\nexit 1\n')
            systemctl.chmod(0o755)
            env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'), PATH=str(bin_dir))
            # User setup intentionally refuses root; emulate the user identity in root-run CI.
            command = [sys.executable, '-c', f"import runpy, os; os.geteuid = lambda: 1000; runpy.run_path({str(HELPER)!r}, run_name='__main__')", 'user-setup']
            result = subprocess.run(command, env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(override.exists())
            self.assertEqual(len(list(override.parent.glob('*.before-*'))), 1)
            systemctl.write_text('#!/bin/sh\nexit 0\n')
            for _ in range(2):
                result = subprocess.run(command, env=env, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(list(override.parent.glob('*.before-*'))), 1)
            override.write_text('custom configuration\n')
            result = subprocess.run(command, env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(override.read_text(), 'custom configuration\n')


if __name__ == '__main__':
    unittest.main()
