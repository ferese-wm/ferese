#!/usr/bin/env python3
"""Build the Ferese homepage and redirect former handbook URLs to the wiki."""
from html import escape
import importlib.util
import json
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / 'build/site'
spec = importlib.util.spec_from_file_location('wiki', ROOT / 'scripts/build-wiki.py')
wiki = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wiki)


def redirect_page(destination):
    return f'''<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Ferese documentation</title>
  <link rel="canonical" href="{escape(destination, quote=True)}">
  <meta http-equiv="refresh" content="0;url={escape(destination, quote=True)}">
  <script>location.replace({json.dumps(destination)} + location.hash);</script>
</head>
<body><p>This guide is on the <a href="{escape(destination, quote=True)}">Ferese wiki</a>.</p></body>
</html>
'''


def main():
    if OUTPUT.exists():
        shutil.rmtree(OUTPUT)
    OUTPUT.mkdir(parents=True)
    for name in ('index.html', 'home.css', 'styles.css'):
        shutil.copy2(ROOT / 'site' / name, OUTPUT / name)
    (OUTPUT / 'assets').mkdir(exist_ok=True)
    shutil.copytree(ROOT / 'site/assets', OUTPUT / 'assets', dirs_exist_ok=True)
    shutil.copytree(ROOT / 'docs/images', OUTPUT / 'assets', dirs_exist_ok=True)
    shutil.copytree(ROOT / 'assets/branding', OUTPUT / 'assets/branding')
    (OUTPUT / '.nojekyll').touch()
    handbook = OUTPUT / 'docs'
    handbook.mkdir()
    (handbook / 'index.html').write_text(redirect_page(wiki.WIKI))
    for source in wiki.guide_sources():
        if not source.is_relative_to(ROOT / 'docs') or source.stem == 'README':
            continue
        destination = wiki.WIKI + '/' + wiki.page_name(source)
        output = handbook / source.relative_to(ROOT / 'docs').with_suffix('.html')
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(redirect_page(destination))
    print(f'Built homepage and documentation redirects at {OUTPUT}')


if __name__ == '__main__':
    main()
