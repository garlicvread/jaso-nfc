#!/usr/bin/env python3
"""Check the public guide's local links and basic markup without network access.

Run with Python 3.11+ from any directory. External URLs are not fetched; links
to this project's Pages origin and repository blob/main files resolve locally.
"""

from __future__ import annotations

import html
from html.parser import HTMLParser
from pathlib import Path
import re
import sys
import tomllib
import unicodedata
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parents[1]
SITE = ROOT / "site"
PAGES_HOST = "garlicvread.github.io"
PAGES_BASE = "/jaso-nfc/"
REPOSITORY = "/garlicvread/jaso-nfc"
PUBLIC_MARKDOWN = ("README.md", "README.en.md", "docs/usage.md", "docs/usage.en.md")


class Markup(HTMLParser):
    """Collect link targets, anchor IDs, and a few actionable a11y mistakes."""

    def __init__(self, source: str, *, document: bool = False):
        super().__init__(convert_charrefs=True)
        self.ids: set[str] = set()
        self.links: list[tuple[int, str]] = []
        self.errors: list[tuple[int, str]] = []
        self.lang: str | None = None
        self.buttons: list[dict] = []
        self.active_buttons: list[dict] = []
        self.feed(source)
        self.close()
        if document and not self.lang:
            self.errors.append((1, "add a nonempty lang attribute to <html>"))
        for button in self.buttons:
            labels = button["labels"].split()
            for label in labels:
                if label not in self.ids:
                    self.errors.append((button["line"], f"button aria-labelledby target #{label} is missing"))
            if not (button["name"].strip() or button["text"].strip() or labels):
                self.errors.append((button["line"], "button needs text or an accessible label"))

    def handle_starttag(self, tag: str, attributes: list[tuple[str, str | None]]) -> None:
        attrs = dict(attributes)
        line = self.getpos()[0]
        identifier = attrs.get("id")
        if identifier is not None:
            if not identifier or identifier in self.ids:
                self.errors.append((line, f"empty or duplicate id {identifier!r}"))
            self.ids.add(identifier)
        if tag == "a" and attrs.get("name"):
            self.ids.add(attrs["name"])
        if tag == "html":
            self.lang = (attrs.get("lang") or "").strip()
        for attribute in ("href", "src"):
            if attribute in attrs:
                self.links.append((line, attrs[attribute] or ""))
        if tag == "img":
            if "alt" not in attrs:
                self.errors.append((line, "image needs alt text (alt=\"\" for a decorative image)"))
            for button in self.active_buttons:
                button["text"] += attrs.get("alt") or ""
        if tag == "button":
            button = {
                "line": line,
                "name": attrs.get("aria-label") or attrs.get("title") or "",
                "labels": attrs.get("aria-labelledby") or "",
                "text": "",
            }
            self.buttons.append(button)
            self.active_buttons.append(button)

    def handle_startendtag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        self.handle_starttag(tag, attrs)
        self.handle_endtag(tag)

    def handle_endtag(self, tag: str) -> None:
        if tag == "button" and self.active_buttons:
            self.active_buttons.pop()

    def handle_data(self, data: str) -> None:
        for button in self.active_buttons:
            button["text"] += data


def without_fences(source: str) -> str:
    """Keep source line numbers while ignoring fenced examples."""
    lines = []
    fence = None
    for line in source.splitlines(keepends=True):
        marker = re.match(r"^ {0,3}(`{3,}|~{3,})", line)
        if fence:
            if marker and marker[1][0] == fence[0] and len(marker[1]) >= len(fence):
                fence = None
            lines.append("\n" if line.endswith("\n") else "")
        elif marker:
            fence = marker[1]
            lines.append("\n" if line.endswith("\n") else "")
        else:
            lines.append(line)
    return "".join(lines)


def markdown_ids(source: str) -> set[str]:
    source = without_fences(source)
    ids = Markup(source).ids
    used: set[str] = set()
    lines = source.splitlines()
    for number, line in enumerate(lines):
        heading = re.match(r"^ {0,3}#{1,6}\s+(.+?)(?:\s+#+)?\s*$", line)
        if heading:
            title = heading[1]
        elif number + 1 < len(lines) and line.strip() and re.fullmatch(r" {0,3}(?:=+|-+)\s*", lines[number + 1]):
            title = line.strip()
        else:
            continue
        title = re.sub(r"!?\[([^]]*)\]\([^)]*\)", r"\1", title)
        title = html.unescape(re.sub(r"<[^>]+>", "", title)).lower()
        title = title.replace("`", "").replace("*", "")
        slug = "".join(char for char in title if char in "-_ " or unicodedata.category(char)[0] not in "PS").replace(" ", "-")
        identifier = slug
        suffix = 0
        while identifier in used:
            suffix += 1
            identifier = f"{slug}-{suffix}"
        used.add(identifier)
        ids.add(identifier)
    return ids


def markdown_links(source: str) -> list[tuple[int, str]]:
    """Read inline, reference, autolink, and embedded HTML link destinations.

    This deliberately checks destinations rather than imposing a Markdown
    style. Parentheses in inline paths are balanced, and titles are ignored.
    """
    source = without_fences(source)
    source = re.sub(r"(`+).*?\1", lambda match: " " * len(match[0]), source)
    links = Markup(source).links
    # Reference definitions are destinations even if their label is unused.
    for match in re.finditer(r"(?m)^ {0,3}\[[^]\n]+\]:\s*(?:<([^>]+)>|(\S+))", source):
        links.append((source.count("\n", 0, match.start()) + 1, match[1] or match[2]))
    for match in re.finditer(r"\]\(\s*", source):
        start = match.end()
        position = start
        if start < len(source) and source[start] == "<":
            position = source.find(">", start + 1)
            if position < 0:
                continue
            target = source[start + 1:position]
        else:
            depth = 0
            while position < len(source):
                char = source[position]
                if char == "\\" and position + 1 < len(source):
                    position += 2
                    continue
                if char == "(":
                    depth += 1
                elif char == ")":
                    if depth == 0:
                        break
                    depth -= 1
                elif char.isspace() and depth == 0:
                    break
                position += 1
            target = source[start:position]
        links.append((source.count("\n", 0, match.start()) + 1, re.sub(r"\\([() ])", r"\1", target)))
    for match in re.finditer(r"<(https?://[^<>\s]+)>", source):
        links.append((source.count("\n", 0, match.start()) + 1, match[1]))
    return links


def main() -> int:
    errors: list[str] = []
    documents: dict[Path, tuple[set[str], list[tuple[int, str]]]] = {}

    def error(path: Path, line: int, message: str) -> None:
        errors.append(f"{path.relative_to(ROOT)}:{line}: {message}")

    try:
        version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    except (OSError, ValueError, KeyError) as exc:
        print(f"Cargo.toml: cannot read package version: {exc}", file=sys.stderr)
        return 1

    def read_document(path: Path) -> tuple[set[str], list[tuple[int, str]]]:
        if path not in documents:
            source = path.read_text(encoding="utf-8")
            if path.suffix.lower() == ".md":
                documents[path] = (markdown_ids(source), markdown_links(source))
            else:
                markup = Markup(source, document=path.suffix.lower() == ".html")
                documents[path] = (markup.ids, markup.links)
                for line, message in markup.errors:
                    error(path, line, message)
        return documents[path]

    paths = sorted(SITE.rglob("*.html"))
    for required in (SITE / "index.html", SITE / "en/index.html", *(ROOT / name for name in PUBLIC_MARKDOWN)):
        if not required.is_file():
            error(required, 1, "required public entry page is missing")
        elif required not in paths:
            paths.append(required)

    for path in paths:
        _, links = read_document(path)
        for line, raw_target in links:
            target = html.unescape(raw_target).strip()
            try:
                url = urlsplit(target)
            except ValueError as exc:
                error(path, line, f"invalid URL {target!r}: {exc}")
                continue
            url_path = unquote(url.path)
            release_prefix = REPOSITORY + "/releases/download/"
            if url.hostname == "github.com" and url_path.startswith(release_prefix):
                expected = f"{release_prefix}v{version}/Jaso-NFC-{version}-arm64-local.dmg"
                if url_path not in (expected, expected + ".sha256"):
                    error(path, line, f"download {target!r} does not match Cargo.toml {version}; expected https://github.com{expected}")
            pages_link = url.hostname == PAGES_HOST
            blob_prefix = REPOSITORY + "/blob/main/"
            repository_link = url.hostname == "github.com" and url_path.startswith(blob_prefix)
            if (url.scheme or url.netloc) and not (pages_link or repository_link):
                continue
            in_site = pages_link or (path.is_relative_to(SITE) and not repository_link)
            boundary = SITE if in_site else ROOT
            if repository_link:
                destination = ROOT / url_path.removeprefix(blob_prefix)
            elif in_site and (pages_link or url_path.startswith("/")):
                if url_path == PAGES_BASE.rstrip("/"):
                    url_path += "/"
                if not url_path.startswith(PAGES_BASE):
                    error(path, line, f"site link {target!r} must stay below {PAGES_BASE}")
                    continue
                destination = SITE / url_path.removeprefix(PAGES_BASE)
            elif url_path:
                destination = (ROOT if url_path.startswith("/") else path.parent) / url_path.lstrip("/")
            else:
                destination = path
            destination = destination.resolve()
            if not destination.is_relative_to(boundary):
                error(path, line, f"link {target!r} escapes {boundary.relative_to(ROOT) or '.'}")
                continue
            if destination.is_dir() and in_site:
                destination /= "index.html"
            if not destination.exists():
                error(path, line, f"broken link {target!r}: {destination.relative_to(ROOT)} does not exist")
                continue
            fragment = unquote(url.fragment)
            if fragment and destination.suffix.lower() in (".md", ".html", ".svg"):
                ids, _ = read_document(destination)
                if fragment not in ids:
                    error(path, line, f"broken anchor {target!r}: #{fragment} is missing from {destination.relative_to(ROOT)}")

    if errors:
        print("Public documentation checks failed:", file=sys.stderr)
        for message in errors:
            print(f"  {message}", file=sys.stderr)
        return 1
    print(f"Public documentation checks passed: {len(paths)} pages; local links, anchors, markup, and v{version} download URLs.")
    print("External HTTP availability is checked separately after publishing.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
