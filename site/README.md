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

`docs/images/ferese-icon-glow.svg` is the supplied transparent orbit logo with
cyan-to-blue gradients, edge highlights and three SVG drop-shadow passes. The
website intro, headers and footers, handbook and README use this self-contained
SVG on its original 1024×1024 canvas. Display it square so the native paths and
glow margins remain intact; do not crop it to the flat logo's viewBox.

`docs/images/ferese-icon.svg` retains the flat blue-gradient orbit, identical to
`packaging/icons/ferese.svg`. Its white and dark variants retain the 1058:650
aspect ratio. The shell's symbolic SVG uses the same paths with `currentColor`
for the selected desktop theme. The glow variant is specific to the site and
README; desktop icons and favicons keep the flat variant.

`site/assets/favicon-32.png` is a transparent 32×32 PNG, and
`site/assets/favicon.ico` contains transparent 16×16, 32×32 and 48×48 images.
They are rendered from the canonical gradient SVG and centered in square canvases
without stretching the orbit. These checked-in assets are copied by the normal
site build; icon generation adds no build dependency. Keep homepage and handbook
favicon references relative so they resolve under the GitHub Pages project path.

`assets/wallpapers/ferese.png` is the supplied 1672×941 RGB orbit wallpaper,
stored without resizing or re-encoding. The installer bundles it at
`wallpapers/ferese.png`; the desktop default and lock-screen fallback use this
PNG. Custom desktop and lock-screen wallpaper paths continue to take precedence.
Existing screenshots retain the previous artwork.
