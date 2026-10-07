#!/usr/bin/env python3
"""Export Ferese's symbolic icon and favicons from its canonical SVG."""
from io import BytesIO
from pathlib import Path
import xml.etree.ElementTree as ET

import cairo
import gi
from PIL import Image

gi.require_version('Rsvg', '2.0')
from gi.repository import Rsvg

ROOT = Path(__file__).resolve().parent.parent
BRANDING = ROOT / 'assets/branding'
SVG = 'http://www.w3.org/2000/svg'
ET.register_namespace('', SVG)


def render(path, size):
    handle = Rsvg.Handle.new_from_file(str(path))
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, *size)
    viewport = Rsvg.Rectangle()
    viewport.x = viewport.y = 0
    viewport.width, viewport.height = size
    handle.render_document(cairo.Context(surface), viewport)
    output = BytesIO()
    surface.write_to_png(output)
    output.seek(0)
    with Image.open(output) as image:
        return image.convert('RGBA')


def main():
    symbolic = ET.parse(BRANDING / 'ferese.svg').getroot()
    for defs in symbolic.findall(f'{{{SVG}}}defs'):
        symbolic.remove(defs)
    for path in symbolic.iter(f'{{{SVG}}}path'):
        path.set('fill', 'currentColor')
    ET.indent(symbolic, space='  ')
    (BRANDING / 'ferese-symbolic.svg').write_text(ET.tostring(symbolic, encoding='unicode') + '\n')

    icon = render(BRANDING / 'ferese.svg', (512, 512))
    icon.resize((32, 32), Image.Resampling.LANCZOS).save(ROOT / 'site/assets/favicon-32.png')
    icon.save(ROOT / 'site/assets/favicon.ico', sizes=[(16, 16), (32, 32), (48, 48)])

    print('Updated symbolic icon and favicons from the canonical Fe mark.')


if __name__ == '__main__':
    main()
