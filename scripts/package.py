#!/usr/bin/env python3
"""Prepare offline release sources and stage the complete distro package."""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def run(*args, cwd=ROOT, env=None):
    return subprocess.check_output(args, cwd=cwd, env=env, text=True)


def version():
    return tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']


def check_tag(tag, release_version):
    if not re.fullmatch(r'\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?', release_version):
        raise ValueError(f'Unsupported release version: {release_version}')

    if tag and tag != f'v{release_version}':
        raise ValueError(f'Tag {tag} does not match Cargo version {release_version}')


def licenses(destination, metadata):
    destination.mkdir(parents=True, exist_ok=True)
    report = []
    for package in sorted(metadata['packages'], key=lambda p: (p['name'], p['version'])):
        if package['source'] is None:
            continue

        directory = Path(package['manifest_path']).parent
        source_hash = hashlib.sha256(package['source'].encode()).hexdigest()[:8]
        name = f"{package['name']}-{package['version']}-{source_hash}"
        files = set()
        search = [directory]
        if package['source'].startswith('git+'):
            for ancestor in directory.parents:
                search.append(ancestor)
                if (ancestor / '.git').exists():
                    break

        for parent in search:
            for pattern in ('LICENSE*', 'LICENCE*', 'COPYING*', 'NOTICE*', 'UNLICENSE*'):
                files.update(parent.glob(pattern))

        if package.get('license_file'):
            files.add(directory / package['license_file'])

        for path in sorted(files):
            if path.is_file():
                repeated = sum(other.name == path.name for other in files) > 1
                filename = f'{path.parent.name}-{path.name}' if repeated else path.name
                target = destination / name / filename
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, target)

        report.append({'name': package['name'], 'version': package['version'],
                       'source': package['source'], 'license': package['license'],
                       'notice_directory': name})

    (destination / 'dependencies.json').write_text(json.dumps(report, indent=2) + '\n')


def stage(destdir, binary_dir, pam):
    destdir = Path(destdir).absolute()
    if destdir == Path('/') or destdir.is_symlink():
        raise ValueError('DESTDIR must be a private package staging directory')

    if destdir.exists() and any(destdir.iterdir()):
        raise ValueError('DESTDIR must be empty')

    inventory = json.loads((ROOT / 'scripts/installer/inventory.json').read_text())
    planned = []
    links = []
    for entry in inventory['files']:
        if entry['target'].startswith('pam/'):
            continue

        if entry['source'].startswith('target/release/'):
            sources = [Path(binary_dir).absolute() / Path(entry['source']).name]
        else:
            sources = sorted(ROOT.glob(entry['source']))
        if not sources:
            raise ValueError(f"Missing package asset: {entry['source']}")

        for source in sources:

            target = entry['target'].replace('{name}', source.name)
            if 'destination' in entry:
                target = entry['destination'].replace('/usr/local/', '/usr/').lstrip('/')
            elif target.startswith(('wallpapers/', 'licenses/')):
                prefix = 'usr/share/ferese' if target.startswith('wallpapers/') else 'usr/share'
                if target.startswith('licenses/'):
                    target = 'licenses/ferese/' + target.removeprefix('licenses/')
                target = f'{prefix}/{target}'
            else:
                target = f'usr/libexec/ferese/{target}'

            planned.append((source, target, int(entry['mode'], 8)))
            if entry.get('link'):
                links.append((f'usr/bin/{source.name}', f'/usr/libexec/ferese/{source.name}'))

    planned.extend([(ROOT / f'packaging/pam.d/ferese-lock.{pam}', 'etc/pam.d/ferese-lock', 0o644),
                    (ROOT / 'LICENSE', 'usr/share/licenses/ferese/LICENSE', 0o644),
                    (ROOT / 'README.md', 'usr/share/doc/ferese/README.md', 0o644)])
    # Validate the complete inventory before writing any part of the package.
    for source, _, _ in planned:
        if not source.is_file():
            raise ValueError(f'Missing package file: {source}')
    for source, target, mode in planned:
        destination = destdir / target
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        destination.chmod(mode)
        if source.suffix in ('.desktop', '.service', '.target', '.portal', '.conf'):
            text = destination.read_text().replace('/usr/local/lib/ferese/current/', '/usr/libexec/ferese/')
            destination.write_text(text.replace('/usr/local/', '/usr/'))

    bundled = ROOT / 'bundled-licenses'
    if bundled.exists():
        shutil.copytree(bundled, destdir / 'usr/share/licenses/ferese/dependencies', dirs_exist_ok=True)
    else:
        metadata = json.loads(run('cargo', 'metadata', '--locked', '--offline', '--format-version', '1'))
        licenses(destdir / 'usr/share/licenses/ferese/dependencies', metadata)

    for path in (destdir / 'usr/share/licenses/ferese').rglob('*'):
        if path.is_file():
            path.chmod(0o644)

    for path in destdir.rglob('*'):
        if path.is_dir() and not path.is_symlink():
            path.chmod(0o755)

    links.append(('usr/libexec/ferese/wallpapers', '/usr/share/ferese/wallpapers'))
    for target, source in links:
        destination = destdir / target
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.symlink_to(source)


def source(output, tag, offline):
    release_version = version()
    check_tag(tag, release_version)
    output = Path(output).absolute()
    output.mkdir(parents=True, exist_ok=True)
    epoch = int(os.environ.get('SOURCE_DATE_EPOCH', run('git', 'show', '-s', '--format=%ct', 'HEAD').strip()))
    with tempfile.TemporaryDirectory(prefix='ferese-source-') as temporary:
        tree = Path(temporary) / f'ferese-{release_version}'
        tree.mkdir()
        # Include new packaging files in local dry runs; release checkouts are clean.
        files = run('git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard').split('\0')
        for filename in sorted(set(files) - {''}):
            path = ROOT / filename
            if not path.is_file():
                continue

            destination = tree / filename
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, destination)

        fetch = ['cargo', 'fetch', '--locked']
        if offline:
            fetch.append('--offline')

        run(*fetch, cwd=tree)
        metadata = json.loads(run('cargo', 'metadata', '--locked', '--offline', '--format-version', '1', cwd=tree))
        licenses(tree / 'bundled-licenses', metadata)
        # cargo vendor cannot represent dpi 0.1.2 from both crates.io and Git.
        # Preserve source identities with an isolated, credential-free Cargo cache.
        cache = tree / 'cargo-home'
        for package in metadata['packages']:
            package_source = package['source']
            if package_source is None:
                continue

            manifest = Path(package['manifest_path'])
            if package_source.startswith('registry+'):
                registry = manifest.parent.parent.name
                registry_home = manifest.parent.parent.parent.parent
                name = package['name']
                crate = f"{name}-{package['version']}.crate"
                for relative in (f'cache/{registry}/{crate}', f'index/{registry}/config.json'):
                    original = registry_home / relative
                    destination = cache / 'registry' / relative
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(original, destination)

                if len(name) == 1:
                    index_path = f'1/{name}'
                elif len(name) == 2:
                    index_path = f'2/{name}'
                elif len(name) == 3:
                    index_path = f'3/{name[0]}/{name}'
                else:
                    index_path = f'{name[:2]}/{name[2:4]}/{name}'

                relative = f'index/{registry}/.cache/{index_path}'
                destination = cache / 'registry' / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(registry_home / relative, destination)
            elif package_source.startswith('git+'):
                checkout = next(parent for parent in manifest.parents if parent.parent.name == 'checkouts')
                original = checkout.parent.parent / 'db' / checkout.name
                destination = cache / 'git/db' / checkout.name
                if not destination.exists():
                    shutil.copytree(original, destination)

                # Cargo cannot fetch Git submodules in frozen mode. Carry the
                # completed checkout, including .cargo-ok and submodule objects.
                revision = next(parent for parent in manifest.parents if parent.parent == checkout)
                if (revision / '.gitmodules').exists():
                    ready = cache / 'git/checkouts' / checkout.name / revision.name
                    if not ready.exists():
                        shutil.copytree(revision, ready)
            else:
                raise ValueError(f'Unsupported dependency source: {package_source}')

        environment = dict(os.environ, CARGO_HOME=str(cache))
        run('cargo', 'metadata', '--frozen', '--offline', '--format-version', '1', cwd=tree, env=environment)
        # Cargo recreates these from the checked source archives and Git objects.
        for relative in ('registry/src',):
            shutil.rmtree(cache / relative, ignore_errors=True)

        archive = output / f'ferese-{release_version}.tar.gz'
        with archive.open('wb') as raw, gzip.GzipFile(filename='', fileobj=raw, mode='wb', mtime=epoch) as zipped:
            with tarfile.open(fileobj=zipped, mode='w') as tar:
                for path in [tree, *sorted(tree.rglob('*'))]:
                    info = tar.gettarinfo(str(path), arcname=str(path.relative_to(tree.parent)))
                    info.uid = info.gid = 0
                    info.uname = info.gname = ''
                    info.mtime = epoch
                    # The invoking user's umask must not change release permissions.
                    info.mode = 0o755 if info.isdir() or info.mode & 0o111 else 0o644
                    if info.isfile():
                        with path.open('rb') as contents:
                            tar.addfile(info, contents)
                    else:
                        tar.addfile(info)

        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        recipe = (ROOT / 'packaging/arch/PKGBUILD.in').read_text()
        recipe = recipe.replace('@VERSION@', release_version.replace('-', ''))
        recipe = recipe.replace('@UPSTREAM_VERSION@', release_version).replace('@SHA256@', digest)
        (output / 'PKGBUILD').write_text(recipe)
        spec = (ROOT / 'packaging/fedora/ferese.spec.in').read_text()
        (output / 'ferese.spec').write_text(spec.replace('@VERSION@', release_version.replace('-', '~'))
                                          .replace('@UPSTREAM_VERSION@', release_version))
        (output / 'SHA256SUMS').write_text(f'{digest}  {archive.name}\n')
        print(archive)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    stage_parser = commands.add_parser('stage')
    stage_parser.add_argument('--destdir', required=True)
    stage_parser.add_argument('--binary-dir', default=str(ROOT / 'target/release'))
    stage_parser.add_argument('--pam', choices=['fedora', 'arch'], required=True)
    source_parser = commands.add_parser('source')
    source_parser.add_argument('--output', default='target/dist')
    source_parser.add_argument('--tag')
    source_parser.add_argument('--offline', action='store_true')
    args = parser.parse_args()
    if args.command == 'stage':
        stage(args.destdir, args.binary_dir, args.pam)
    else:
        source(args.output, args.tag, args.offline)


if __name__ == '__main__':
    main()
