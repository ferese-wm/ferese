# Ferese website

The website links to the [GitHub Wiki](https://github.com/ferese-wm/ferese/wiki)
for documentation. Former handbook URLs redirect to the matching wiki pages.

## Local preview

```sh
python3 -m venv .site-venv
.site-venv/bin/pip install -r site/requirements.txt
.site-venv/bin/python scripts/build-site.py
python3 -m http.server 8080 --bind 127.0.0.1 --directory build/site
```

Open http://localhost:8080 for the homepage. Rebuild and refresh the browser
after changing the site sources.

- `site/index.html` and `site/home.css`: homepage and desktop screenshots.
- `site/styles.css`: shared dark appearance, typography and navigation.
- `docs/*.md` and its configuration/development topic directories: guide sources published to the wiki.
- `scripts/build-site.py`: copies homepage assets and writes handbook redirects.
- `scripts/build-wiki.py`: validates and generates wiki pages.

## GitHub Pages

Choose **Settings → Pages → Source → GitHub Actions** to enable publishing, then
open **Actions → Deploy website to GitHub Pages → Run workflow** and choose the
branch to publish. Deployment runs manually, and the expected address is
https://ferese-wm.github.io/ferese/.

The homepage uses relative asset links, so it also works under the `/ferese/`
project path. Each build copies screenshots from `docs/images` into
`build/site`, which is ignored by Git.

For deployment setup, follow [GitHub’s custom workflow
documentation](https://docs.github.com/en/pages/getting-started-with-github-pages/using-custom-workflows-with-github-pages).

## Brand assets

`assets/branding/ferese.svg` is the canonical blue Fe mark used by the README,
website, favicons and installed application icons. Its square 950×950 viewBox is
shared with `ferese-symbolic.svg`, which uses the same shape and `currentColor`
for shell and notification icons. Keep both variants in sync.

Regenerate the symbolic icon and raster favicons after updating the canonical SVG:

```sh
python3 scripts/update-branding.py
```

The exporter requires Python PyGObject, Pycairo, Pillow and librsvg. The generated
`site/assets/favicon-32.png` is 32×32; `site/assets/favicon.ico` contains 16×16,
32×32 and 48×48 images. They are checked in, so the site build needs no renderer.
The links work at both the site root and the GitHub Pages project path.

The light and dark wallpapers live in `assets/wallpapers`. Both are
3840×2160 and are bundled by the installer. Ferese selects the matching image
when no custom wallpaper is set. Settings keeps both defaults available below
the current wallpaper preview.

Screenshots live in `docs/images/screenshots` and are shared by the README and
website. Full captures retain their original 2880×1800 resolution, while the
homepage uses smaller WebP versions for narrower screens.
