#!/usr/bin/env python3
"""Publish repository guides as GitHub Wiki Markdown, preserving their prose."""
import argparse
import json
from pathlib import Path
import re
import subprocess
from urllib.parse import unquote, urlsplit

from markdown_it import MarkdownIt

ROOT = Path(__file__).resolve().parent.parent
REPO = "https://github.com/ferese-wm/ferese"
WIKI = REPO + "/wiki"
MANIFEST = ".ferese-pages.json"
MD = MarkdownIt("commonmark").enable("table")
NAMES = {
    "README": "Home",
    "activities": "Live-Activities",
    "ipc": "IPC",
    "locking": "Lock-Screen",
    "packaging": "Packages-and-Releases",
    "portals": "Desktop-Portals",
    "screen-sharing": "Screen-Sharing-and-Recording",
    "shortcuts": "Shortcuts-and-Gestures",
}
ALIASES = {
    "Appearance": [("Configuration-Appearance", "", "Appearance")],
    "Desktop-Widgets": [("Configuration", "", "Configuration")],
    "Displays": [("Configuration-Outputs", "", "Displays")],
    "Input-and-Bindings": [
        ("Configuration-Input", "", "Input"),
        ("Configuration-Bindings", "", "Commands and bindings"),
    ],
    "Layout-and-Motion": [
        ("Configuration-Layout", "", "Layout"),
        ("Configuration-Animations", "", "Animation configuration"),
        ("Animation-Model", "", "Animation model"),
    ],
    "Services-and-Locking": [
        ("Configuration-Startup-And-Locking", "", "Login items"),
        ("Lock-Screen", "", "Lock screen"),
        ("Desktop-Portals", "", "Desktop portals"),
    ],
    "Window-Rules": [("Configuration-Window-Rules", "", "Window rules")],
}
# Inline links/images and reference definitions. Only destinations recognized by
# the Markdown parser are rewritten; examples in code blocks remain untouched.
LINK = re.compile(r"(?P<code>(?P<ticks>`+).*?(?P=ticks))|(?P<prefix>\]\(|^ {0,3}\[[^]\n]+\]:\s*)(?P<url><[^>\n]+>|[^\s)]+)", re.MULTILINE)


def page_name(source):
    if source == ROOT / "site/README.md":
        return "Website-Development"
    if source == ROOT / "crates/ferese-theme/README.md":
        return "Theme-Components"
    if source.parent == ROOT / "docs/development":
        return "Development-" + "-".join(word.capitalize() for word in source.stem.split("-"))
    if source.parent == ROOT / "docs/configuration":
        return "Configuration-" + "-".join(word.capitalize() for word in source.stem.split("-"))
    return NAMES.get(source.stem, "-".join(word.capitalize() for word in source.stem.split("-")))


def heading_ids(text):
    counts, result = {}, set()
    tokens = MD.parse(text)
    for index, token in enumerate(tokens):
        if token.type != "heading_open":
            continue
        children = tokens[index + 1].children or []
        title = "".join(child.content for child in children if child.type in ("text", "code_inline", "image"))
        base = re.sub(r"[^\w\s-]", "", title.lower()).replace(" ", "-")
        count = counts.get(base, 0)
        anchor = base if count == 0 else f"{base}-{count}"
        while anchor in result:
            count += 1
            anchor = f"{base}-{count}"
        counts[base] = count + 1
        result.add(anchor)
    return result


def destinations(text):
    return {
        child.attrGet("href" if child.type == "link_open" else "src")
        for token in MD.parse(text)
        for child in token.children or []
        if child.type in ("link_open", "image")
    }


def rewrite(text, source, pages, revision):
    replacements = {}
    for href in destinations(text):
        url = urlsplit(href)
        if url.scheme or url.netloc:
            continue
        target = (source.parent / unquote(url.path)).resolve() if url.path else source
        if url.path and (not target.is_relative_to(ROOT) or not target.is_file()):
            raise ValueError(f"{source.relative_to(ROOT)}: missing link target {href}")
        if target in pages:
            if url.fragment and unquote(url.fragment) not in heading_ids(target.read_text()):
                raise ValueError(f"{source.relative_to(ROOT)}: missing heading {href}")
            destination = WIKI + "/" + pages[target]
        elif target.suffix == ".md":
            raise ValueError(f"{source.relative_to(ROOT)}: unpublished guide {href}")
        else:
            relative = target.relative_to(ROOT).as_posix()
            destination = f"{REPO}/raw/{revision}/{relative}" if target.suffix in (".png", ".jpg", ".webp", ".svg") else f"{REPO}/blob/{revision}/{relative}"
        if not url.path:
            destination = ""
        if url.query:
            destination += "?" + url.query
        if url.fragment:
            destination += "#" + url.fragment
        replacements[href] = destination

    def replace(match):
        if match["code"] is not None:
            return match[0]
        raw = match["url"]
        angled = raw.startswith("<")
        href = raw[1:-1] if angled else raw
        if href not in replacements:
            return match[0]
        replacement = replacements[href]
        return match["prefix"] + ("<" + replacement + ">" if angled else replacement)

    # Use parser line ranges to protect both fenced and indented code blocks.
    lines = text.splitlines(keepends=True)
    protected = set()
    for token in MD.parse(text):
        if token.type in ("fence", "code_block"):
            protected.update(range(*token.map))
    return "".join(line if number in protected else LINK.sub(replace, line) for number, line in enumerate(lines))


def guide_sources():
    return (sorted((ROOT / "docs").glob("*.md"))
            + sorted((ROOT / "docs/configuration").glob("*.md"))
            + sorted((ROOT / "docs/development").glob("*.md"))
            + [ROOT / "site/README.md", ROOT / "crates/ferese-theme/README.md"])


def render(revision):
    sources = guide_sources()
    pages = {source.resolve(): page_name(source) for source in sources}
    if len(set(pages.values())) != len(pages):
        raise ValueError("Wiki page names must be unique")
    output = {}
    for source, name in pages.items():
        text = rewrite(source.read_text(), source, pages, revision)
        output[name + ".md"] = text
    for alias, links in ALIASES.items():
        targets = []
        for name, anchor, title in links:
            if name + ".md" not in output:
                raise ValueError(f"Alias points to absent page {name}")
            if anchor and anchor not in heading_ids(output[name + ".md"]):
                raise ValueError(f"Alias points to absent heading {name}#{anchor}")
            targets.append(f"- [{title}]({WIKI}/{name}" + ("#" + anchor if anchor else "") + ")")
        output[alias + ".md"] = "# " + alias.replace("-", " ") + "\n\nSee the current guides:\n\n" + "\n".join(targets) + "\n"
    home = output["Home.md"]
    # The documentation index is also the navigation source. Keep descriptions
    # on Home, and use its grouped link list for the Wiki sidebar.
    sidebar = [f"[Home]({WIKI})"]
    for line in home.splitlines():
        if line.startswith("## "):
            sidebar.extend(["", line, ""])
        elif line.startswith("- "):
            link = re.match(r"- \[[^]]+\]\([^)]+\)", line)
            if link:
                sidebar.append(link[0])
    output["_Sidebar.md"] = "\n".join(sidebar) + "\n"
    output["Home.md"] += f"\nEdit the guide sources in the [main repository]({REPO}/tree/main/docs).\n"
    return output


def write_pages(output, destination):
    destination.mkdir(parents=True, exist_ok=True)
    manifest = destination / MANIFEST
    previous = json.loads(manifest.read_text()) if manifest.exists() else []
    if not isinstance(previous, list) or any(not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]+\.md", name) for name in previous):
        raise ValueError("Invalid wiki page manifest")
    for name in previous:
        if name not in output:
            (destination / name).unlink(missing_ok=True)
    for name, text in output.items():
        (destination / name).write_text(text)
    manifest.write_text(json.dumps(sorted(output), indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=ROOT / "build/wiki")
    args = parser.parse_args()
    destination = args.output.resolve()
    if destination == ROOT or destination.is_relative_to(ROOT / "docs"):
        parser.error("output must not overwrite documentation sources")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    output = render(revision)  # Validate every guide before touching destination.
    write_pages(output, destination)
    print(f"Built {len(output)} wiki pages at {destination}")


if __name__ == "__main__":
    main()
