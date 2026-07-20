#!/usr/bin/env python3
"""
mirror_n64brew_wiki.py -- build and maintain the local N64brew Wiki mirror.

Background
==========

RustyN64 uses the `N64brew Wiki <https://n64brew.dev/wiki/Main_Page>`_ as its
primary hardware-behaviour reference (alongside the immutable
``ref-docs/research-report.md``). This script produces a gitignored offline
mirror at ``n64brew_wiki/`` so chip work can be done without round-tripping to
the live site, and so agent/grep passes can search the corpus as plain text.

Unlike the RustyNES ``nesdev_wiki`` mirror -- which came from an HTTrack-style
crawl and needed a four-script repair pipeline afterwards -- n64brew.dev runs a
current MediaWiki (1.45.x) with a fully open ``api.php``. That means the mirror
can be built correctly in one pass from structured data instead of being
scraped and then patched:

* page HTML comes from ``action=parse`` (templates already expanded, so the
  Template: and Module: namespaces never need mirroring),
* the wikitext source comes from the same call,
* images come from ``list=allimages`` with their real upstream URLs,
* revision IDs are recorded so later runs are incremental.

Layout produced
===============

::

    n64brew_wiki/
      README.md               (hand-written; not touched by this script)
      manifest.json           page list, revids, title->path map
      html/<Title>.xhtml      browsable, links + images rewritten to local
      html/Category/<X>.xhtml non-main namespaces nest under their namespace
      markdown/<Title>.md     text conversion for grep / agent context
      wikitext/<Title>.wiki   raw MediaWiki source
      images/<File>           original-resolution media

Main-namespace subpages keep their natural hierarchy: the upstream title
``64DD/Commands`` becomes ``html/64DD/Commands.xhtml``.

Usage
=====

::

    python3 scripts/mirror_n64brew_wiki.py                 # full sync
    python3 scripts/mirror_n64brew_wiki.py --dry-run       # enumerate only
    python3 scripts/mirror_n64brew_wiki.py --limit 10      # smoke test
    python3 scripts/mirror_n64brew_wiki.py --refresh       # only changed revids
    python3 scripts/mirror_n64brew_wiki.py --verify        # audit local refs
    python3 scripts/mirror_n64brew_wiki.py --no-images     # skip media

Pure Python 3 standard library. ``pandoc`` is used for HTML->Markdown when it
is on PATH; without it the Markdown files fall back to the raw wikitext, which
greps equally well.

The N64brew Wiki is published under CC BY-SA 4.0. This mirror is for local
reference only -- upstream remains the authoritative copy. Do not commit it.
"""

from __future__ import annotations

import argparse
import html
import json
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

API = "https://n64brew.dev/w/api.php"
SITE = "https://n64brew.dev"
UA = "RustyN64-wiki-mirror/1.0 (https://github.com/doublegate/RustyN64; offline reference mirror)"

# Namespaces worth mirroring. Template (10) and Module (828) are deliberately
# excluded: action=parse returns fully-expanded HTML, so their content is
# already baked into the pages that use them.
NAMESPACES = {
    0: "",            # main -- lands at the root of each tree
    6: "File",        # file description pages (licensing/attribution)
    12: "Help",
    14: "Category",
}

# Characters that are legal in MediaWiki titles but hostile in paths. "/" is
# intentionally absent -- it is what gives us the nested subpage layout.
UNSAFE = str.maketrans({c: "_" for c in ':*?"<>|\\'})

MIRROR_DIRS = ("html", "markdown", "wikitext", "images")


def log(msg: str) -> None:
    print(msg, flush=True)


def api_get(params: dict, retries: int = 4) -> dict:
    """GET api.php with backoff. Returns the decoded JSON body."""
    params = {**params, "format": "json", "formatversion": "2"}
    url = f"{API}?{urllib.parse.urlencode(params)}"
    delay = 1.0
    for attempt in range(1, retries + 1):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=60) as resp:
                return json.loads(resp.read().decode("utf-8"))
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
            if attempt == retries:
                raise
            log(f"    retry {attempt}/{retries - 1} after {exc.__class__.__name__}: {exc}")
            time.sleep(delay)
            delay *= 2
    raise RuntimeError("unreachable")


def safe_relpath(title: str, ns: int) -> str:
    """Map an upstream title to a mirror-relative path stem.

    ``64DD/Commands`` (ns 0)      -> ``64DD/Commands``
    ``Category:Hardware`` (ns 14) -> ``Category/Hardware``
    """
    stem = title
    prefix = NAMESPACES.get(ns, "")
    if prefix and stem.startswith(prefix + ":"):
        stem = stem[len(prefix) + 1:]
    parts = [p.translate(UNSAFE).strip() or "_" for p in stem.split("/")]
    rel = "/".join(parts)
    return f"{prefix}/{rel}" if prefix else rel


def enumerate_pages(limit: int | None = None) -> list[dict]:
    """List every page in the mirrored namespaces."""
    pages: list[dict] = []
    for ns in sorted(NAMESPACES):
        cont: dict = {}
        while True:
            data = api_get({
                "action": "query", "list": "allpages",
                "apnamespace": ns, "aplimit": "500", **cont,
            })
            batch = data.get("query", {}).get("allpages", [])
            for p in batch:
                pages.append({"title": p["title"], "ns": ns, "pageid": p["pageid"]})
            if "continue" in data:
                cont = data["continue"]
            else:
                break
        label = NAMESPACES[ns] or "Main"
        log(f"  ns {ns:>3} ({label}): "
            f"{sum(1 for p in pages if p['ns'] == ns)} pages")
    pages.sort(key=lambda p: (p["ns"], p["title"]))
    return pages[:limit] if limit else pages


def fetch_page(title: str) -> dict | None:
    """Fetch expanded HTML + wikitext + revid for one page.

    ``redirects=1`` is tried first so a redirect yields its target's content.
    A *broken* redirect (target never created) makes the API report
    ``missingtitle`` for a page that demonstrably exists, so fall back to
    parsing the redirect page itself -- that preserves the stub and keeps
    inbound links from other pages resolving.
    """
    props = "text|wikitext|revid|displaytitle"
    data = api_get({"action": "parse", "page": title,
                    "prop": props, "redirects": "1"})
    if data.get("error", {}).get("code") == "missingtitle":
        data = api_get({"action": "parse", "page": title, "prop": props})
        if "error" not in data:
            log(f"    - {title}: broken redirect, kept as stub")
    if "error" in data:
        log(f"    ! {title}: {data['error'].get('code')}")
        return None
    return data.get("parse")


def strip_chrome(body: str) -> str:
    """Remove MediaWiki UI furniture that is meaningless in a static mirror.

    The per-section ``[edit] | [edit source]`` spans are the worst offender:
    they survive into Markdown and put two dead links under every heading.
    """
    # Match on the link target rather than on class nesting: the wrapper markup
    # differs between MediaWiki skins/versions, but a section-edit anchor always
    # points at (ve)action=edit with a section index.
    body = re.sub(r'<a\b[^>]*[?&](?:amp;)?(?:ve)?action=edit[^>]*section=\d+[^>]*>.*?</a>',
                  "", body, flags=re.S)
    body = re.sub(r'<span class="mw-editsection-(?:divider|bracket)"[^>]*>.*?</span>',
                  "", body, flags=re.S)
    body = re.sub(r'<span class="mw-editsection"[^>]*>\s*</span>', "", body)
    body = re.sub(r'<div class="printfooter".*?</div>', "", body, flags=re.S)
    return body


def rewrite_links(body: str, rel_stem: str, title_to_path: dict[str, str],
                  image_names: set[str]) -> str:
    """Point /wiki/... and image URLs at local siblings.

    Every mirrored file computes its own ``../`` prefix from its depth, so the
    tree can be browsed from any starting page.
    """
    # Page links stay inside html/, so they only climb out of subpage nesting.
    # Images live at the mirror root (../images/), one level further up.
    depth = rel_stem.count("/")
    up = "../" * depth
    img_up = "../" * (depth + 1)

    def to_local(target: str) -> str | None:
        target = urllib.parse.unquote(target).replace("_", " ").strip()
        target = target.split("#")[0]
        if not target:
            return None
        path = title_to_path.get(target)
        if path is None:
            # Try first-letter capitalisation (MediaWiki's "first-letter" case).
            path = title_to_path.get(target[:1].upper() + target[1:])
        return path

    def href_sub(m: re.Match) -> str:
        attr, quote, target = m.group(1), m.group(2), m.group(3)
        frag = ""
        if "#" in target:
            target, frag = target.split("#", 1)
            frag = "#" + frag
        local = to_local(target)
        if local is None:
            return f'{attr}={quote}{SITE}/wiki/{target}{frag}{quote}'
        # Titles legitimately contain spaces; percent-encode so the href is a
        # valid URI, but keep "/" so subpage nesting stays navigable.
        enc = urllib.parse.quote(local, safe="/")
        return f'{attr}={quote}{up}{enc}.xhtml{frag}{quote}'

    body = re.sub(r'(href)=(["\'])/wiki/([^"\'#]*(?:#[^"\']*)?)\2', href_sub, body)

    def img_sub(m: re.Match) -> str:
        attr, quote, url = m.group(1), m.group(2), m.group(3)
        name = url.rstrip("/").split("/")[-1]
        name = urllib.parse.unquote(name)
        # Thumbnails look like .../thumb/1/1e/Foo.png/220px-Foo.png -- strip the
        # NNNpx- prefix to land on the full-resolution original.
        name = re.sub(r"^\d+px-", "", name)
        if name in image_names:
            # Must match the on-disk name produced by fetch_images(), which
            # applies the same UNSAFE translation before writing.
            enc = urllib.parse.quote(name.translate(UNSAFE))
            return f'{attr}={quote}{img_up}images/{enc}{quote}'
        return f'{attr}={quote}{url}{quote}'

    # The Miraheze/wikitide CDN serves media protocol-relative ("//static..."),
    # so the scheme must be optional here or every image stays remote.
    body = re.sub(r'(src)=(["\'])((?:https?:)?//[^"\']+|/w/images/[^"\']+)\2',
                  img_sub, body)
    # srcset carries retina variants that would still hit the network.
    body = re.sub(r'\ssrcset=(["\'])[^"\']*\1', "", body)
    # Anything still root-relative (Special:MathShowImage renders, /w/ action
    # endpoints, skin assets) has no local equivalent -- point it upstream so it
    # degrades to a working online link instead of a dead relative path.
    body = re.sub(r'(href|src)=(["\'])(/[^/"\'][^"\']*)\2',
                  lambda m: f'{m.group(1)}={m.group(2)}{SITE}{m.group(3)}{m.group(2)}',
                  body)
    return body


PAGE_TEMPLATE = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" lang="en">
<head>
<meta charset="UTF-8" />
<title>{title} -- N64brew Wiki (local mirror)</title>
<link rel="canonical" href="{canonical}" />
<meta name="x-mirror-revid" content="{revid}" />
<style>
body {{ max-width: 60em; margin: 2em auto; padding: 0 1em;
        font-family: system-ui, sans-serif; line-height: 1.5; }}
table {{ border-collapse: collapse; }}
td, th {{ border: 1px solid #999; padding: 0.2em 0.5em; }}
pre {{ overflow-x: auto; background: #f4f4f4; padding: 0.5em; }}
img {{ max-width: 100%; height: auto; }}
.mirror-banner {{ background: #ffd; border: 1px solid #cc0;
                  padding: 0.5em; margin-bottom: 1.5em; font-size: 0.9em; }}
</style>
</head>
<body>
<div class="mirror-banner">
Local mirror of <a href="{canonical}">{title}</a> from the N64brew Wiki
(CC BY-SA 4.0). Revision {revid}. Upstream is authoritative.
</div>
<h1>{title}</h1>
{body}
</body>
</html>
"""


def to_markdown(html_body: str, wikitext: str, title: str, canonical: str) -> str:
    """HTML -> Markdown via pandoc when available; wikitext otherwise."""
    header = (
        f"# {title}\n\n"
        f"> Local mirror of [{title}]({canonical}) from the N64brew Wiki "
        f"(CC BY-SA 4.0). Upstream is authoritative.\n\n"
    )
    if shutil.which("pandoc"):
        try:
            out = subprocess.run(
                ["pandoc", "-f", "html", "-t", "gfm-raw_html", "--wrap=none"],
                input=html_body, capture_output=True, text=True, timeout=120,
            )
            if out.returncode == 0 and out.stdout.strip():
                return header + out.stdout
        except (subprocess.SubprocessError, OSError):
            pass
    return header + "```mediawiki\n" + wikitext + "\n```\n"


def fetch_images(root: Path, dry_run: bool) -> set[str]:
    """Download every uploaded file at original resolution."""
    names: set[str] = set()
    cont: dict = {}
    entries: list[dict] = []
    while True:
        data = api_get({"action": "query", "list": "allimages",
                        "ailimit": "500", "aiprop": "url|size", **cont})
        entries.extend(data.get("query", {}).get("allimages", []))
        if "continue" in data:
            cont = data["continue"]
        else:
            break
    log(f"  {len(entries)} media files listed")
    if dry_run:
        return {e["name"] for e in entries}

    outdir = root / "images"
    outdir.mkdir(parents=True, exist_ok=True)
    for i, e in enumerate(entries, 1):
        name = e["name"]
        names.add(name)
        dest = outdir / name.translate(UNSAFE)
        if dest.exists() and dest.stat().st_size == e.get("size", -1):
            continue
        try:
            req = urllib.request.Request(e["url"], headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=120) as resp:
                dest.write_bytes(resp.read())
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            log(f"    ! image {name}: {exc}")
            continue
        if i % 25 == 0:
            log(f"    {i}/{len(entries)} media")
        time.sleep(0.1)
    return names


def verify(root: Path) -> int:
    """Audit every local-pointing href/src; report targets that do not exist."""
    html_root = root / "html"
    if not html_root.is_dir():
        log("no html/ tree -- nothing to verify")
        return 1
    missing: list[str] = []
    checked = 0
    for f in sorted(html_root.rglob("*.xhtml")):
        text = f.read_text(encoding="utf-8", errors="replace")
        for m in re.finditer(r'(?:href|src)=(["\'])([^"\']+)\1', text):
            ref = m.group(2).split("#")[0]
            # Skip any absolute URI. The scheme grammar allows digits, "+",
            # "-" and "." after the first letter -- MediaWiki emits "mw-data:"
            # for TemplateStyles, which a bare [a-z]+ would miss.
            if not ref or re.match(r"^[a-z][a-z0-9+.-]*:", ref, re.I) or ref.startswith("//"):
                continue
            checked += 1
            target = (f.parent / urllib.parse.unquote(ref)).resolve()
            if not target.exists():
                missing.append(f"{f.relative_to(root)} -> {ref}")
    log(f"verify: {checked} local refs checked, {len(missing)} unresolved")
    for m in missing[:40]:
        log(f"  ! {m}")
    if len(missing) > 40:
        log(f"  ... and {len(missing) - 40} more")
    return 0 if not missing else 2


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("--root", default="n64brew_wiki", help="mirror directory")
    ap.add_argument("--dry-run", action="store_true", help="enumerate, write nothing")
    ap.add_argument("--limit", type=int, help="cap page count (smoke test)")
    ap.add_argument("--refresh", action="store_true",
                    help="only re-fetch pages whose revid changed")
    ap.add_argument("--verify", action="store_true", help="audit local refs and exit")
    ap.add_argument("--no-images", action="store_true", help="skip media download")
    ap.add_argument("--delay", type=float, default=0.25, help="seconds between calls")
    args = ap.parse_args()

    root = Path(args.root)
    if args.verify:
        return verify(root)

    log(f"N64brew Wiki mirror -> {root}/")
    log("enumerating pages...")
    pages = enumerate_pages(args.limit)
    log(f"  {len(pages)} pages selected")

    title_to_path = {p["title"]: safe_relpath(p["title"], p["ns"]) for p in pages}

    log("listing media...")
    image_names = set() if args.no_images and not args.dry_run else fetch_images(root, True)

    if args.dry_run:
        log("dry-run: no files written")
        for p in pages[:20]:
            log(f"  {p['title']}  ->  html/{title_to_path[p['title']]}.xhtml")
        return 0

    manifest_path = root / "manifest.json"
    old = {}
    if args.refresh and manifest_path.exists():
        old = {e["title"]: e for e in json.loads(manifest_path.read_text())["pages"]}

    for d in MIRROR_DIRS:
        (root / d).mkdir(parents=True, exist_ok=True)

    if not args.no_images:
        log("downloading media...")
        image_names = fetch_images(root, False)

    log("fetching pages...")
    entries, failed, skipped = [], 0, 0
    for i, p in enumerate(pages, 1):
        title, rel = p["title"], title_to_path[p["title"]]
        parsed = fetch_page(title)
        if parsed is None:
            failed += 1
            continue
        revid = parsed.get("revid", 0)
        if args.refresh and old.get(title, {}).get("revid") == revid:
            skipped += 1
            entries.append(old[title])
            continue

        canonical = f"{SITE}/wiki/{urllib.parse.quote(title.replace(' ', '_'))}"
        body = rewrite_links(strip_chrome(parsed["text"]), rel, title_to_path,
                             image_names)
        wikitext = parsed.get("wikitext", "")

        for sub, ext, content in (
            ("html", ".xhtml", PAGE_TEMPLATE.format(
                title=html.escape(title), canonical=canonical,
                revid=revid, body=body)),
            ("wikitext", ".wiki", wikitext),
            ("markdown", ".md", to_markdown(body, wikitext, title, canonical)),
        ):
            dest = root / sub / (rel + ext)
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(content, encoding="utf-8")

        entries.append({"title": title, "ns": p["ns"], "pageid": p["pageid"],
                        "revid": revid, "path": rel})
        if i % 25 == 0:
            log(f"  {i}/{len(pages)} pages")
        time.sleep(args.delay)

    manifest_path.write_text(json.dumps({
        "source": SITE, "api": API,
        "license": "CC BY-SA 4.0",
        "namespaces": {str(k): v or "Main" for k, v in NAMESPACES.items()},
        "page_count": len(entries), "image_count": len(image_names),
        "pages": entries,
    }, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    log(f"done: {len(entries)} pages ({skipped} unchanged), "
        f"{len(image_names)} media, {failed} failed")
    log(f"manifest: {manifest_path}")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
