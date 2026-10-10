#!/usr/bin/env python3
"""Build release bundles and apply recoverable, serialized system installations."""
import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import uuid

REPO = Path(__file__).resolve().parents[2]
INVENTORY = json.loads(Path(__file__).with_name('inventory.json').read_text())
PREFIX = '/usr/local/lib/ferese'
ID = re.compile(r'[A-Za-z0-9][A-Za-z0-9._-]*\Z')


def fail(message):
    raise ValueError(message)


def release_id(value):
    if not ID.fullmatch(value):
        fail(f'Invalid release ID: {value}')

    return value


def digest(path):
    with path.open('rb') as stream:
        checksum = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            checksum.update(block)

        return checksum.hexdigest()


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def ensure_directory(path, mode=0o755):
    missing = []
    cursor = path
    while not cursor.exists():
        if cursor.is_symlink():
            fail(f'Refusing dangling directory symlink: {cursor}')
        missing.append(cursor)
        cursor = cursor.parent

    for directory in reversed(missing):
        # This CLI is single-threaded. Apply the requested mode at creation so
        # a process killed before chmod cannot leave a umask-restricted parent.
        previous_umask = os.umask(0)
        try:
            directory.mkdir(mode=mode)
        finally:
            os.umask(previous_umask)
        directory.chmod(mode)
        sync_dir(directory.parent)


def atomic_bytes(path, data, mode=0o644):
    ensure_directory(path.parent)
    fd, temporary = tempfile.mkstemp(prefix='.ferese-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            os.fchmod(stream.fileno(), mode)
            stream.flush()
            os.fsync(stream.fileno())

        os.replace(temporary, path)
        sync_dir(path.parent)
    finally:
        if os.path.lexists(temporary):
            os.unlink(temporary)


def atomic_copy(path, source, mode):
    ensure_directory(path.parent)
    fd, temporary = tempfile.mkstemp(prefix='.ferese-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(source_fd, 'rb') as stream:
                if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                    fail(f'Expected regular file: {source}')
                shutil.copyfileobj(stream, output)

            os.fchmod(output.fileno(), mode)
            output.flush()
            os.fsync(output.fileno())

        os.replace(temporary, path)
        sync_dir(path.parent)
    finally:
        if os.path.lexists(temporary):
            os.unlink(temporary)


def sync_tree(path):
    # Persist new directory entries as well as file contents before publication.
    for directory, _, _ in os.walk(path, topdown=False):
        sync_dir(Path(directory))


def atomic_json(path, data, mode=0o600):
    atomic_bytes(path, (json.dumps(data, indent=2, sort_keys=True) + '\n').encode(), mode)


def record(rule, name=None):
    return {key: (value.format(name=name) if key == 'target' else value)
            for key, value in rule.items() if key != 'source'}


def validate_bundle(bundle, *, installed=False):
    if bundle.is_symlink() or not bundle.is_dir():
        fail(f'Bundle must be a directory, not a symlink: {bundle}')

    for path in bundle.rglob('*'):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            fail(f'Unexpected bundle entry: {path}')

    manifest = json.loads((bundle / 'manifest.json').read_text())
    if manifest.get('format') != 1:
        fail('Unsupported bundle format')

    release_id(manifest['release'])
    expected = {rule['target']: record(rule) for rule in INVENTORY['files'] if '*' not in rule['source']}
    seen = set()
    for item in manifest['files']:
        target = item['target']
        allowed = expected.get(target)
        # Older releases bundled this retired source asset. Keep their manifests
        # verifiable for upgrades and rollback without adding it to new bundles.
        if allowed is None and installed and re.fullmatch(r'wallpapers/[A-Za-z0-9._-]+\.jpe?g', target):
            allowed = {'target': target, 'mode': '0644'}
        if allowed is None and target == 'wallpapers/ferese.svg':
            allowed = {'target': target, 'mode': '0644'}
        if allowed is None and re.fullmatch(r'licenses/theme-[A-Za-z0-9._-]+\.txt', target):
            allowed = {'target': target, 'mode': '0644'}

        if allowed is None or {k: v for k, v in item.items() if k != 'sha256'} != allowed or target in seen:
            fail(f'Invalid inventory entry: {target}')

        path = bundle / target
        if not path.is_file() or digest(path) != item['sha256']:
            fail(f'Checksum mismatch: {target}')
        if stat.S_IMODE(path.stat().st_mode) != int(item['mode'], 8):
            fail(f'Incorrect file mode: {target}')

        seen.add(target)

    actual = {str(path.relative_to(bundle)) for path in bundle.rglob('*') if path.is_file()}
    required = expected.keys()
    if installed:
        # Installed releases may predate the appearance wallpapers and license.
        # New bundles must include them; upgrades and rollback verify the older
        # release's recorded files without requiring these later additions.
        required = required - {'wallpapers/ferese-wallpaper-dark.png', 'wallpapers/ferese-wallpaper-light.png',
                               'licenses/Ferese-LICENSE.txt'}

    if not required <= seen or actual != seen | {'manifest.json'}:
        fail('Bundle is incomplete or contains unlisted files')

    return manifest


def bundle(args):
    release_id(args.release_id)
    source = args.source_root.resolve()
    output = args.output.absolute()
    if output.exists() or output.is_symlink():
        fail(f'Bundle already exists: {output}')

    ensure_directory(output.parent)
    with tempfile.TemporaryDirectory(prefix='.bundle-', dir=output.parent) as temporary:
        staging = Path(temporary) / 'release'
        ensure_directory(staging)
        files = []
        for rule in INVENTORY['files']:
            sources = sorted(source.glob(rule['source']))
            if not sources:
                fail(f'Missing bundle input: {rule["source"]}')

            for origin in sources:
                if origin.is_symlink() or not origin.is_file():
                    fail(f'Bundle input must be a regular file: {origin}')
                if rule['source'].startswith('target/release/') and not os.access(origin, os.X_OK):
                    fail(f'Bundle input is not executable: {origin}')

                item = record(rule, origin.name)
                destination = staging / item['target']
                atomic_copy(destination, origin, int(item['mode'], 8))
                item['sha256'] = digest(destination)
                files.append(item)

        def git(*command):
            result = subprocess.run(['git', '-C', str(source), *command], capture_output=True, text=True)
            return result.stdout.strip() if result.returncode == 0 else None

        status = git('status', '--porcelain')
        manifest = dict(format=1, release=args.release_id, commit=git('rev-parse', 'HEAD'),
                        dirty=None if status is None else bool(status), features=args.features,
                        created_utc=time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), files=files)
        atomic_json(staging / 'manifest.json', manifest, 0o644)
        validate_bundle(staging)
        sync_tree(staging)
        os.rename(staging, output)
        sync_dir(output.parent)

    print(f'Prepared bundle: {output}')


class Installation:
    def __init__(self, root):
        self.root = root.resolve()
        self.base = self.path(PREFIX)
        self.transaction = self.base / '.transaction'

    def path(self, absolute):
        if not absolute.startswith('/') or '..' in Path(absolute).parts:
            fail(f'Invalid installation path: {absolute}')

        path = self.root / absolute.lstrip('/')
        for parent in path.parents:
            if parent == self.root:
                break
            if parent.is_symlink():
                fail(f'Refusing symlinked parent: {parent}')

        return path

    @contextlib.contextmanager
    def lock(self):
        ensure_directory(self.base)
        lock_path = self.path(PREFIX + '/.install.lock')
        fd = os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                fail('Another Ferese installation is in progress')

            yield
        finally:
            os.close(fd)

    def pointer(self, name):
        path = self.path(PREFIX + '/' + name)
        if not os.path.lexists(path):
            return None
        if not path.is_symlink():
            fail(f'Unmanaged release pointer: {path}')

        target = os.readlink(path)
        if not target.startswith('releases/') or not ID.fullmatch(target[len('releases/'):]):
            fail(f'Invalid release pointer: {path}')

        return target[len('releases/'):]

    def state(self, absolute):
        path = self.path(absolute)
        if path.is_symlink():
            return {'kind': 'link', 'target': os.readlink(path)}
        if not path.exists():
            return {'kind': 'absent'}
        if not path.is_file():
            fail(f'Unmanaged non-file path: {path}')

        return {'kind': 'file', 'sha256': digest(path), 'mode': stat.S_IMODE(path.stat().st_mode)}

    def manifest(self, release):
        directory = self.path(PREFIX + '/releases/' + release_id(release))
        if not (directory / 'manifest.json').is_file():
            fail(f'Release {release} predates verified bundles; select a bundled release for rollback')

        return validate_bundle(directory, installed=True)

    def current_publication(self, current):
        if not current:
            return {}

        directory = self.path(PREFIX + '/releases/' + current)
        if (directory / 'manifest.json').exists():
            return self.publication(self.manifest(current))

        # The previous installer retained exact originals for these integration files.
        # Use those ownership receipts for the first upgrade, never the live file itself.
        if directory.is_symlink() or not (directory / 'installer-files').is_dir():
            fail(f'Release has no installation manifest: {current}')

        entries = {}
        for rule in INVENTORY['files']:
            if rule.get('link'):
                target = rule['target']
                entries['/usr/local/bin/' + target] = (
                    {'kind': 'link', 'target': PREFIX + '/current/' + target}, None, 'managed')
            if 'destination' in rule:
                saved = directory / 'installer-files' / rule['source'].removeprefix('packaging/')
                if saved.is_file() and not saved.is_symlink():
                    entries[rule['destination']] = (
                        {'kind': 'file', 'sha256': digest(saved), 'mode': int(rule['mode'], 8)},
                        None, rule['policy'])

        return entries

    def publication(self, manifest):
        entries = {}
        for item in manifest['files']:
            if item.get('link'):
                destination = '/usr/local/bin/' + item['target']
                entries[destination] = ({'kind': 'link', 'target': PREFIX + '/current/' + item['target']}, None, 'managed')
            if 'destination' in item:
                source = PREFIX + '/releases/' + manifest['release'] + '/' + item['target']
                entries[item['destination']] = ({'kind': 'file', 'sha256': item['sha256'],
                                                'mode': int(item['mode'], 8)}, source, item['policy'])

        return entries

    def plan(self, manifest, replace_portal=False, remove=False):
        current = self.pointer('current')
        self.pointer('previous')
        previous = self.current_publication(current)
        desired = {} if remove else self.publication(manifest)
        operations = []
        for destination in sorted(previous.keys() | desired.keys()):
            old = self.state(destination)
            new, source, policy = desired.get(destination, ({'kind': 'absent'}, None, 'managed'))
            owned = previous.get(destination, (None,))[0]
            if old != {'kind': 'absent'} and old != owned and old != new:
                if not (replace_portal and policy == 'portal' and old['kind'] == 'file'):
                    fail(f'Unmanaged or modified file: {destination}')

            if old != new:
                operations.append(dict(path=destination, old=old, new=new, source=source))

        pam_path = '/etc/pam.d/ferese-lock'
        if not remove and not os.path.lexists(self.path(pam_path)):
            if self.path('/etc/pam.d/system-auth').is_file():
                policy = 'fedora'
            elif all(self.path('/etc/pam.d/' + name).is_file() for name in ('common-auth', 'common-account')):
                policy = 'debian'
            elif self.path('/etc/pam.d/login').is_file():
                policy = 'generic'
            else:
                fail('No supported PAM stack; install /etc/pam.d/ferese-lock first')

            item = next(item for item in manifest['files'] if item['target'] == 'pam/' + policy)
            operations.append(dict(path=pam_path, old={'kind': 'absent'},
                                   new={'kind': 'file', 'sha256': item['sha256'], 'mode': 0o644},
                                   source=PREFIX + '/releases/' + manifest['release'] + '/' + item['target']))

        for name, target in [('previous', self.pointer('previous') if current == manifest['release'] and not remove else current),
                             ('current', None if remove else manifest['release'])]:
            old = self.state(PREFIX + '/' + name)
            new = {'kind': 'link', 'target': 'releases/' + target} if target else {'kind': 'absent'}
            if old != new:
                operations.append(dict(path=PREFIX + '/' + name, old=old, new=new, source=None))

        return operations

    def write_state(self, absolute, state, source=None):
        path = self.path(absolute)
        ensure_directory(path.parent)
        if state['kind'] == 'absent':
            path.unlink(missing_ok=True)
            sync_dir(path.parent)
        elif state['kind'] == 'file':
            if digest(source) != state['sha256']:
                fail(f'Recovery source checksum mismatch: {source}')

            atomic_copy(path, source, state['mode'])
        else:
            temporary = path.parent / ('.ferese-link-' + uuid.uuid4().hex)
            try:
                temporary.symlink_to(state['target'])
                os.replace(temporary, path)
                sync_dir(path.parent)
            finally:
                temporary.unlink(missing_ok=True)

    def archive(self, journal):
        history = self.path(PREFIX + '/transactions')
        ensure_directory(history, 0o700)
        os.rename(self.transaction, history / journal['id'])
        sync_dir(history)
        sync_dir(self.base)

    def recover(self):
        if not self.transaction.exists():
            return

        if self.transaction.is_symlink():
            fail('Refusing symlinked recovery transaction')

        journal = json.loads((self.transaction / 'journal.json').read_text())
        if journal['phase'] != 'committed':
            # Check all paths before restoring anything; preserve later administrator edits.
            for operation in journal['operations']:
                actual = self.state(operation['path'])
                if actual not in (operation['old'], operation['new']):
                    fail(f'Recovery conflict at {operation["path"]}; transaction retained at {self.transaction}')

            for index, operation in reversed(list(enumerate(journal['operations']))):
                self.write_state(operation['path'], operation['old'], self.transaction / 'backup' / str(index))

            journal['phase'] = 'rolled-back'
            atomic_json(self.transaction / 'journal.json', journal)
            print('Recovered interrupted installation; restored previous files')

        self.archive(journal)

    def apply(self, operations):
        if not operations:
            print('Already installed; no system changes needed')
            return

        journal = dict(id=time.strftime('%Y%m%dT%H%M%S', time.gmtime()) + '-' + uuid.uuid4().hex,
                       phase='applying', operations=operations)
        with tempfile.TemporaryDirectory(prefix='.prepare-', dir=self.base) as temporary:
            staging = Path(temporary) / 'transaction'
            staging.mkdir(mode=0o700)
            for index, operation in enumerate(operations):
                if self.state(operation['path']) != operation['old']:
                    fail(f'File changed during preparation: {operation["path"]}')
                if operation['old']['kind'] == 'file':
                    backup = staging / 'backup' / str(index)
                    atomic_copy(backup, self.path(operation['path']), 0o600)
                    if digest(backup) != operation['old']['sha256']:
                        fail(f'File changed while backing up: {operation["path"]}')

            atomic_json(staging / 'journal.json', journal)
            sync_tree(staging)
            os.rename(staging, self.transaction)
            sync_dir(self.base)

        try:
            for operation in operations:
                if self.state(operation['path']) != operation['old']:
                    fail(f'File changed during installation: {operation["path"]}')

                source = self.path(operation['source']) if operation['source'] else None
                self.write_state(operation['path'], operation['new'], source)

            journal['phase'] = 'committed'
            atomic_json(self.transaction / 'journal.json', journal)
        except BaseException:
            self.recover()
            raise

        self.archive(journal)

    def stage(self, origin, manifest):
        releases = self.path(PREFIX + '/releases')
        if releases.is_symlink():
            fail(f'Refusing symlinked release directory: {releases}')

        ensure_directory(releases)
        destination = releases / manifest['release']
        if os.path.lexists(destination):
            if self.manifest(manifest['release']) != manifest:
                fail(f'Release ID already belongs to a different bundle: {manifest["release"]}')

            return

        with tempfile.TemporaryDirectory(prefix='.stage-', dir=releases) as temporary:
            staging = Path(temporary) / 'release'
            ensure_directory(staging)
            for item in manifest['files']:
                atomic_copy(staging / item['target'], origin / item['target'], int(item['mode'], 8))

            atomic_json(staging / 'manifest.json', manifest, 0o644)
            validate_bundle(staging)
            sync_tree(staging)
            os.rename(staging, destination)
            sync_dir(releases)


def system_command(args):
    if args.root.resolve() == Path('/') and os.geteuid() != 0 and not args.dry_run:
        fail('System installation requires sudo or pkexec')

    installation = Installation(args.root)
    if args.dry_run:
        if installation.transaction.exists():
            fail('An interrupted transaction requires recovery before planning')

        if args.command == 'recover':
            print('No interrupted transaction')
            return
        if args.command == 'install':
            manifest = validate_bundle(args.bundle)
        else:
            selected = (installation.pointer('current') if args.command == 'uninstall' else
                        args.release or installation.pointer('previous'))
            manifest = installation.manifest(selected or fail('No release selected'))
        operations = installation.plan(manifest, getattr(args, 'replace_portal_config', False), args.command == 'uninstall')
        for operation in operations:
            print(f'{operation["path"]}: {operation["old"]["kind"]} -> {operation["new"]["kind"]}')
        return

    with installation.lock():
        installation.recover()
        if args.command == 'recover':
            return
        if args.command == 'install':
            manifest = validate_bundle(args.bundle)
            # Preflight before retaining a release or changing integration files.
            operations = installation.plan(manifest, args.replace_portal_config)
            installation.stage(args.bundle, manifest)
        elif args.command == 'rollback':
            selected = args.release or installation.pointer('previous') or fail('No previous release')
            manifest = installation.manifest(selected)
            operations = installation.plan(manifest)
        else:
            current = installation.pointer('current') or fail('No installed release')
            manifest = installation.manifest(current)
            operations = installation.plan(manifest, remove=True)

        installation.apply(operations)
        print(f'System {args.command} complete: {manifest["release"]}')


def user_setup(args):
    if os.geteuid() == 0:
        fail('Run user-setup as your normal user')

    config = Path(os.environ.get('XDG_CONFIG_HOME', str(Path.home() / '.config')))
    override = config / 'systemd/user/xdg-desktop-portal-ferese.service.d/override.conf'
    expected = f'[Service]\nExecStart=\nExecStart={Path.home()}/.local/libexec/ferese/xdg-desktop-portal-ferese\n'
    if os.path.lexists(override):
        if override.is_symlink() or override.read_text() != expected:
            fail(f'Custom portal override preserved: {override}; update its ExecStart to {PREFIX}/current/xdg-desktop-portal-ferese')
        if not args.check:
            backup = override.with_name('override.conf.before-' + uuid.uuid4().hex)
            override.rename(backup)
            print(f'Backed up development portal override: {backup}')

    if not args.check:
        subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
        print('User setup complete. Log out and select Ferese to use the installed release.')


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest='command', required=True)
    build = commands.add_parser('bundle')
    build.add_argument('--source-root', type=Path, default=REPO)
    build.add_argument('--output', type=Path, required=True)
    build.add_argument('--release-id', required=True)
    build.add_argument('--features', default='default')
    commands.add_parser('packages')
    verify = commands.add_parser('verify')
    verify.add_argument('bundle', type=Path)
    user = commands.add_parser('user-setup')
    user.add_argument('--check', action='store_true')
    for name in ('install', 'rollback', 'recover', 'uninstall'):
        command = commands.add_parser(name)
        command.add_argument('--root', type=Path, default=Path('/'), help='Destination filesystem root (for packaging/tests)')
        command.add_argument('--dry-run', action='store_true')
        if name == 'install':
            command.add_argument('bundle', type=Path)
            command.add_argument('--replace-portal-config', action='store_true')
        elif name == 'rollback':
            command.add_argument('release', nargs='?')

    return result


def main():
    args = parser().parse_args()
    if args.command == 'bundle':
        bundle(args)
    elif args.command == 'verify':
        manifest = validate_bundle(args.bundle)
        print(f'Verified bundle: {manifest["release"]}')
    elif args.command == 'packages':
        print('\n'.join(INVENTORY['packages']))
    elif args.command == 'user-setup':
        user_setup(args)
    else:
        system_command(args)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f'ferese installer: {error}', file=sys.stderr)
        sys.exit(1)
