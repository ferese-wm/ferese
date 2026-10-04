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

The orbit symbols live in `assets/branding`. The website uses
`ferese-orbit-blue-dark.svg`. The README chooses the light or dark SVG to match
the reader's appearance and uses the flat blue SVG as its fallback. Keep their
1024×1024 canvas square so the orbit and glow are not stretched or cropped.

The favicon uses the flat blue symbol. `site/assets/favicon-32.png` is 32×32;
`site/assets/favicon.ico` contains 16×16, 32×32 and 48×48 images. These files are
checked in, so building the site needs no icon renderer. Their links work at
both the site root and the GitHub Pages project path.

The light and dark orbit wallpapers live in `assets/wallpapers`. Both are
3840×2160 and are bundled by the installer. Ferese selects the matching image
when no custom wallpaper is set. Settings keeps both defaults available below
the current wallpaper preview. Existing screenshots show the previous artwork.
