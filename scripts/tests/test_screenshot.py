#!/usr/bin/env python3
"""Exercise screenshot selection and locking without a Wayland desktop."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / 'packaging/ferese-screenshot'

OUTPUTS = [
    {'name': 'eDP-1', 'enabled': True, 'focused': False,
     'x': 0, 'y': 0, 'width': 1920, 'height': 1080},
    {'name': 'HDMI-A-1', 'enabled': True, 'focused': True,
     'x': 1920, 'y': 0, 'width': 2560, 'height': 1440},
]
FOCUSED_GEOMETRY = '1920,0 2560x1440'

FARESECTL = '''#!/bin/bash
case "$1" in
    -j) [[ "$2" == outputs && "$#" == 2 ]] || exit 1; cat "$TEST_ROOT/outputs.json" ;;
    screenshot)
        printf '%s\\n' "$@" >> "$TEST_ROOT/captures"
        cat "$TEST_ROOT/capture.png"
        ;;
    *) printf 'unexpected feresectl command: %s\\n' "$1" >&2; exit 1 ;;
esac
'''

SATTY = '''#!/bin/bash
if [[ -e /proc/$$/fd/9 ]]; then touch "$TEST_ROOT/inherited-lock"; fi
printf '%s\\n' "$@" > "$TEST_ROOT/satty-args"
printf 'editor\\n' >> "$TEST_ROOT/editors"
touch "$TEST_ROOT/ready"
while [[ ! -e "$TEST_ROOT/release" ]]; do sleep 0.02; done
'''


def fakes():
    return {
        'feresectl': FARESECTL,
        'notify-send': '#!/bin/bash\nexit 0\n',
        'wl-copy': '#!/bin/bash\nexit 0\n',
        'xdg-user-dir': '#!/bin/bash\nprintf "%s" "$TEST_ROOT/Pictures"\n',
        'satty': SATTY,
    }


class ScreenshotTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bins = self.root / 'bin'
        self.bins.mkdir()
        self.env = dict(os.environ, XDG_RUNTIME_DIR=self.directory.name,
                        TEST_ROOT=str(self.root),
                        PATH=f'{self.bins}:{os.environ["PATH"]}')
        self.write_outputs(OUTPUTS)
        self.write_capture(2560, 1440)
        for name, contents in fakes().items():
            path = self.bins / name
            path.write_text(contents)
            path.chmod(0o755)

    def write_outputs(self, outputs):
        (self.root / 'outputs.json').write_text(json.dumps(outputs))

    def write_capture(self, width, height):
        '''Stand in for the real readback with a PNG of known dimensions.'''
        import struct
        import zlib

        def chunk(kind, data):
            body = kind + data
            return (struct.pack('>I', len(data)) + body
                    + struct.pack('>I', zlib.crc32(body)))

        rows = b''.join(b'\x00' + bytes([10, 20, 30, 255]) * width
                        for _ in range(height))
        png = (b'\x89PNG\r\n\x1a\n'
               + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0))
               + chunk(b'IDAT', zlib.compress(rows))
               + chunk(b'IEND', b''))
        (self.root / 'capture.png').write_bytes(png)

    def editor_args(self):
        return (self.root / 'satty-args').read_text().splitlines()

    def editor_size(self):
        args = self.editor_args()
        return args[args.index('--resize') + 1]

    def captures(self):
        log = self.root / 'captures'
        if not log.exists():
            return []
        return log.read_text().splitlines()

    def run_script(self, *args, timeout=3):
        return subprocess.run(['bash', str(SCRIPT), *args], env=self.env,
                              capture_output=True, timeout=timeout)

    def test_active_output_all_outputs_and_editor_lock(self):
        first = subprocess.Popen(['bash', str(SCRIPT), '--full'], env=self.env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 5
            while not (self.root / 'ready').exists():
                if time.monotonic() > deadline or first.poll() is not None:
                    self.fail('First screenshot did not open its editor')
                time.sleep(0.02)
            second = self.run_script('--full')
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual((self.root / 'editors').read_text().splitlines(), ['editor'])
            editor_args = (self.root / 'satty-args').read_text().splitlines()
            self.assertEqual(editor_args[editor_args.index('--app-id') + 1],
                             'dev.ferese.Screenshot')
            # Derived from the 2560x1440 capture, not from the focused output.
            self.assertEqual(self.editor_size(), '1500x844')
            self.assertIn('--no-window-decoration', editor_args)
            self.assertNotIn('--fullscreen', editor_args)
            self.assertFalse((self.root / 'inherited-lock').exists())
            (self.root / 'release').touch()
            _, stderr = first.communicate(timeout=3)
            self.assertEqual(first.returncode, 0, stderr)
            third = self.run_script('--full')
            self.assertEqual(third.returncode, 0, third.stderr)
            self.assertEqual((self.root / 'editors').read_text().splitlines(), ['editor', 'editor'])
            self.assertFalse(list((self.root / 'Pictures/Screenshots').glob('*.png')))
            self.assertFalse(list(self.root.glob('ferese-screenshot-*.png')))
            # Each --full capture asked for the focused output by geometry, not
            # by name, because native capture has no output-name selection.
            self.assertEqual(self.captures()[0:3], ['screenshot', '-g', FOCUSED_GEOMETRY])
            self.assertEqual(self.captures()[3:6], ['screenshot', '-g', FOCUSED_GEOMETRY])
            all_monitors = self.run_script('--all')
            self.assertEqual(all_monitors.returncode, 0, all_monitors.stderr)
            self.assertEqual(len(self.captures()), 7)  # --all passes only "screenshot"
            self.assertEqual(self.captures()[6], 'screenshot')
            captured = self.captures()
            for outputs in [[], [dict(OUTPUTS[0], focused=False), dict(OUTPUTS[1], focused=False)]]:
                self.write_outputs(outputs)
                unavailable = self.run_script('--full')
                self.assertNotEqual(unavailable.returncode, 0)
                self.assertIn(b'Could not determine the active monitor', unavailable.stderr)
                self.assertEqual(self.captures(), captured)
            # An enabled output with no geometry is ambiguous, not a reason to
            # capture everything.
            self.write_outputs([dict(OUTPUTS[1], focused=True, x=None)])
            unknown = self.run_script('--full')
            self.assertNotEqual(unknown.returncode, 0)
            self.assertIn(b'Could not determine the active monitor', unknown.stderr)
            self.assertEqual(self.captures(), captured)
            # Disabled outputs are ignored, so the sole remaining one is used.
            self.write_outputs([dict(OUTPUTS[0], enabled=False, focused=True),
                                dict(OUTPUTS[1], focused=False)])
            single = self.run_script('--full')
            self.assertEqual(single.returncode, 0, single.stderr)
            self.assertEqual(self.captures()[-2:], ['-g', FOCUSED_GEOMETRY])
        finally:
            (self.root / 'release').touch()
            if first.poll() is None:
                first.terminate()
            first.communicate(timeout=3)

    def test_capture_failure_reports_the_compositor_error(self):
        detail = 'invalid_request: The requested region does not intersect any enabled output'
        self.bins.joinpath('feresectl').write_text(
            '#!/bin/bash\nprintf "%s\\n" "' + detail + '" >&2\nexit 1\n')
        result = self.run_script('--all')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(detail.encode(), result.stderr)
        self.assertNotIn(b'must be running', result.stderr)
        self.assertFalse((self.root / 'editors').exists())
        self.assertEqual(list(self.root.glob('ferese-screenshot-*')), [])

    def test_area_selection_is_passed_through(self):
        self.bins.joinpath('slurp').write_text(
            '#!/bin/bash\nprintf "10,-20 300x200"\n')
        self.bins.joinpath('slurp').chmod(0o755)
        (self.root / 'release').touch()
        area = self.run_script()
        self.assertEqual(area.returncode, 0, area.stderr)
        self.assertEqual(self.captures(), ['screenshot', '-g', '10,-20 300x200'])

    def test_every_mode_captures_through_the_native_command(self):
        (self.root / 'release').touch()
        cases = [(['--all'], ['screenshot']),
                 (['--full'], ['screenshot', '-g', FOCUSED_GEOMETRY])]
        for args, expected in cases:
            with self.subTest(args=args):
                before = len(self.captures())
                result = self.run_script(*args)
                self.assertEqual(result.returncode, 0, result.stderr)
                # The fake only records these when the compositor command runs,
                # so a capture taken any other way leaves the log short.
                self.assertEqual(self.captures()[before:], expected)
    def test_the_editor_is_sized_from_the_image_not_the_output(self):
        # The focused output is 2560x1440 throughout, so a size that tracked the
        # output would be identical for every capture. It must follow the image.
        (self.root / 'release').touch()
        expected = {(1920, 1080): '1500x844',  # capped by width, aspect kept
                    (2560, 1440): '1500x844',
                    (640, 480): '1227x920',    # height cap binds before 2x upscale
                    (3000, 200): '1500x360'}  # extreme aspect still bounded
        for (width, height), size in expected.items():
            with self.subTest(size=f'{width}x{height}'):
                self.write_capture(width, height)
                result = self.run_script('--all')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.editor_size(), size)

    def test_an_unreadable_capture_falls_back_to_a_default_size(self):
        (self.root / 'capture.png').write_bytes(b'not a png')
        (self.root / 'release').touch()
        result = self.run_script('--all')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.editor_size(), '1000x700')


if __name__ == '__main__':
    unittest.main()
