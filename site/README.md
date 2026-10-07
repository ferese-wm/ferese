# Ferese website and handbook

The website's handbook is built from the project's Markdown guides, so the website
and repository share the same documentation.

## Local preview

```sh
python3 -m venv .site-venv
.site-venv/bin/pip install -r site/requirements.txt
.site-venv/bin/python scripts/build-site.py
python3 -m http.server 8080 --bind 127.0.0.1 --directory build/site
```

Open http://localhost:8080 for the homepage or http://localhost:8080/docs/ for the
handbook. Rebuild and refresh the browser after changing the site sources or guides.

- `site/index.html` and `site/styles.css`: homepage and shared visual design.
- `site/content/index.md`: handbook introduction.
- `docs/*.md`: guides; published pages are listed in `scripts/build-site.py`.
- `scripts/build-site.py`: renders Markdown, resolves links, generates section
  navigation, previous/next links, and the search index.
- `site/app.js`: handbook search and code copying. Reading and navigation work
  without JavaScript. Search is local to the browser, with no third-party service.

## GitHub Pages

Choose **Settings → Pages → Source → GitHub Actions** to publish through the Pages
workflow. It runs when site or documentation changes reach `main`; you can also
start it manually. The expected address is https://ferese-wm.github.io/ferese/.

The site uses relative links, including for handbook search, so it also works under the
`/ferese/` project path. Each build copies screenshots from `docs/images` into
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
website. They use lossless WebP at the original 2880×1800 resolution.
