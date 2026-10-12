# Wiki publishing

The [GitHub Wiki](https://github.com/ferese-wm/ferese/wiki) is the documentation
entry point. Guide sources stay in this repository so changes can be reviewed
with the code they describe. Edit the sources rather than the generated wiki pages.

## Update a guide

Edit `docs/*.md`, add new guides to `docs/README.md`, and submit a pull request.
The getting-started guide and the website and theme crate READMEs are also published. The publisher rewrites
relative guide links to wiki pages, checks headings, and links source files and
images to the source commit. It leaves code examples unchanged.

Preview the generated Markdown locally:

```sh
python3 -m venv .site-venv
.site-venv/bin/pip install -r site/requirements.txt
.site-venv/bin/python scripts/build-wiki.py
.site-venv/bin/python -m unittest discover -s scripts/tests -p test_wiki.py -v
```

Pages are written to `build/wiki`. A missing page, file or heading fails the
build before any destination page is changed.

## Publish from main

The **Publish documentation to GitHub Wiki** workflow runs when documentation
changes reach main. It can also be run manually on main. It checks out the latest
main, validates the guides, and pushes changed pages to the wiki repository.
Repeated runs with identical sources create no new wiki commit.

The publisher keeps a manifest of its pages. Removing a guide removes its
previously generated page; pages outside that manifest are left alone. Old wiki
page names have links to the current references.

## Publisher access

The workflow uses a dedicated token stored in the `WIKI_TOKEN` Actions secret.
It must be able to push to `ferese-wm/ferese.wiki.git`. The workflow fails with a
setup message if the secret is absent. The current repository policy disables
deploy keys, so the publisher cannot use that route.

Create a dedicated publishing token with access to the wiki, then add it under
**Settings → Secrets and variables → Actions → New repository secret** as
`WIKI_TOKEN`. Do not store the token in the source repository or reuse a personal
SSH key. To rotate it, replace the secret and confirm a successful workflow run
before revoking the previous token.

## Publish locally

With Git access to the wiki repository:

```sh
git clone git@github.com:ferese-wm/ferese.wiki.git /tmp/ferese-wiki
.site-venv/bin/python scripts/build-wiki.py --output /tmp/ferese-wiki
git -C /tmp/ferese-wiki diff --stat
git -C /tmp/ferese-wiki add -- '*.md' .ferese-pages.json
git -C /tmp/ferese-wiki commit -m 'Update documentation'
git -C /tmp/ferese-wiki push
```

Build from a committed source checkout. Generated pages include links to that
commit, so it must be available on GitHub before the wiki is published.
