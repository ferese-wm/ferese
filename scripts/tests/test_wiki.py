"""Wiki publication must preserve examples and resolve links before writing."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
PARSER_AVAILABLE = importlib.util.find_spec('markdown_it') is not None
if PARSER_AVAILABLE:
    spec = importlib.util.spec_from_file_location('wiki', ROOT / 'scripts/build-wiki.py')
    wiki = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(wiki)


@unittest.skipUnless(PARSER_AVAILABLE, 'Install site/requirements.txt for wiki checks')
class WikiTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'docs').mkdir()
        self.source = self.root / 'docs/intro.md'
        self.source.write_text('# Intro\n\n## Local heading\n')
        self.config = self.root / 'docs/configuration.md'
        self.config.write_text('# Configuration\n\n## With `code` & punctuation!\n\n## Again\n\n## Again\n')
        self.pages = {self.source: 'Intro', self.config: 'Configuration'}
        self.patch = mock.patch.object(wiki, 'ROOT', self.root)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def rewrite(self, text):
        return wiki.rewrite(text, self.source, self.pages, 'deadbeef')

    def test_guide_links_and_duplicate_heading_anchors(self):
        text = '[Reference](configuration.md#with-code--punctuation) and [again](configuration.md#again-1).\n'
        result = self.rewrite(text)
        self.assertIn(wiki.WIKI + '/Configuration#with-code--punctuation', result)
        self.assertIn(wiki.WIKI + '/Configuration#again-1', result)

    def test_local_anchor_and_external_link_unchanged(self):
        text = '[Here](#local-heading) and [Elsewhere](https://example.org/a?q=1#fragment).\n'
        self.assertEqual(self.rewrite(text), text)

    def test_source_files_and_images_are_pinned_to_source_commit(self):
        (self.root / 'docs/image.webp').write_bytes(b'image')
        (self.root / 'example.kdl').write_text('layout {}')
        result = self.rewrite('![Desktop](image.webp) [Example](../example.kdl)\n')
        self.assertIn(wiki.REPO + '/raw/deadbeef/docs/image.webp', result)
        self.assertIn(wiki.REPO + '/blob/deadbeef/example.kdl', result)

    def test_fenced_indented_and_inline_code_are_preserved(self):
        example = '[Config](configuration.md)'
        text = f'{example}\n\n```md\n{example}\n```\n\n    {example}\n\nUse `{example}` as an example.\n'
        result = self.rewrite(text)
        self.assertIn(wiki.WIKI + '/Configuration', result)
        self.assertIn(f'```md\n{example}\n```', result)
        self.assertIn(f'    {example}', result)
        self.assertIn(f'`{example}`', result)

    def test_reference_links_and_angle_destinations(self):
        result = self.rewrite('[Config][ref]\n\n[ref]: <configuration.md> "Title"\n')
        self.assertIn('[ref]: <' + wiki.WIKI + '/Configuration> "Title"', result)

    def test_missing_file_missing_anchor_and_unpublished_markdown_fail(self):
        for target in ('missing.md', 'configuration.md#missing'):
            with self.subTest(target=target), self.assertRaises(ValueError):
                self.rewrite('[Missing](' + target + ')')
        (self.root / 'unpublished.md').write_text('# Unpublished')
        with self.assertRaises(ValueError):
            self.rewrite('[Missing](../unpublished.md)')

    def test_failed_validation_does_not_replace_any_destination_page(self):
        destination = self.root / 'published'
        destination.mkdir()
        (destination / 'Home.md').write_text('Previously published')
        with mock.patch.object(sys, 'argv', ['build-wiki.py', '--output', str(destination)]), mock.patch.object(wiki, 'render', side_effect=ValueError('bad link')), mock.patch.object(wiki.subprocess, 'check_output', return_value='deadbeef'):
            with self.assertRaises(ValueError):
                wiki.main()
        self.assertEqual((destination / 'Home.md').read_text(), 'Previously published')
        self.assertEqual(list(destination.iterdir()), [destination / 'Home.md'])

    def test_stale_generated_pages_removed_and_manual_pages_preserved(self):
        destination = self.root / 'published'
        wiki.write_pages({'Old.md': '# Old', 'Home.md': '# Home'}, destination)
        (destination / 'Manual.md').write_text('Manual page')
        wiki.write_pages({'Home.md': '# New home'}, destination)
        self.assertFalse((destination / 'Old.md').exists())
        self.assertEqual((destination / 'Manual.md').read_text(), 'Manual page')
        self.assertEqual(json.loads((destination / wiki.MANIFEST).read_text()), ['Home.md'])

    def test_manifest_cannot_delete_files_outside_destination(self):
        destination = self.root / 'published'
        destination.mkdir()
        (destination / wiki.MANIFEST).write_text('["../keep.md"]')
        (self.root / 'keep.md').write_text('Keep')
        with self.assertRaises(ValueError):
            wiki.write_pages({'Home.md': '# New'}, destination)
        self.assertEqual((self.root / 'keep.md').read_text(), 'Keep')
        self.assertFalse((destination / 'Home.md').exists())

    def test_repeated_generation_has_identical_content(self):
        destination = self.root / 'published'
        pages = {'Home.md': '# Home', '_Sidebar.md': '[Home](Home)'}
        wiki.write_pages(pages, destination)
        before = {p.name: p.read_bytes() for p in destination.iterdir()}
        wiki.write_pages(pages, destination)
        self.assertEqual(before, {p.name: p.read_bytes() for p in destination.iterdir()})

    def test_every_repository_guide_is_published_and_navigable(self):
        with mock.patch.object(wiki, 'ROOT', ROOT):
            output = wiki.render('deadbeef')
            sources = sorted((ROOT / 'docs').glob('*.md')) + [ROOT / 'site/README.md', ROOT / 'crates/ferese-theme/README.md']
            sidebar = output['_Sidebar.md']
            self.assertEqual([line for line in sidebar.splitlines() if line.startswith('## ')], ['## Usage', '## Configuration', '## Development'])
            self.assertNotIn('Wiki-Publishing.md', output)
            self.assertNotIn('Getting-Started.md', output)
            for source in sources:
                name = wiki.page_name(source)
                with self.subTest(source=source):
                    self.assertIn(name + '.md', output)
                    if name != 'Home':
                        self.assertIn(wiki.WIKI + '/' + name, sidebar)
            for name, content in output.items():
                for href in wiki.destinations(content):
                    self.assertFalse(href.endswith('.md') and not href.startswith('https://'), (name, href))

    def test_homepage_redirects_keep_old_paths_and_fragments(self):
        subprocess.run([sys.executable, str(ROOT / 'scripts/build-site.py')], check=True, capture_output=True)
        destination = ROOT / 'build/site/docs/configuration.html'
        content = destination.read_text()
        self.assertIn(wiki.WIKI + '/Configuration', content)
        self.assertIn('location.hash', content)
        self.assertIn('http-equiv="refresh"', content)
        self.assertIn('<a href="' + wiki.WIKI + '/Configuration">', content)
        self.assertTrue((ROOT / 'build/site/assets/screenshots/ferese-media.webp').is_file())


if __name__ == '__main__':
    unittest.main()
