#!/usr/bin/env python3
"""Validate generated page links, anchors, search targets, and release-update markers."""
from collections import Counter
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit
import importlib.util
import json
import re
import sys
import struct
import xml.etree.ElementTree as ET
from build import BASE, PAGES

ROOT = Path(__file__).resolve().parent.parent

class Page(HTMLParser):
    def __init__(self, path):
        super().__init__()
        self.ids = []
        self.links = []
        self.h1 = 0
        self.main = 0
        self.feed(path.read_text())

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if 'id' in attrs:
            self.ids.append(attrs['id'])
        if tag == 'h1':
            self.h1 += 1
        if tag == 'main':
            self.main += 1
        for attr in ('href', 'src'):
            if attr in attrs:
                self.links.append(attrs[attr])

pages = {path.resolve(): Page(path) for path in [ROOT / 'index.html', *sorted((ROOT / 'docs').glob('*.html'))]}
errors = []

def check_link(source, link):
    url = urlsplit(link)
    if url.scheme or url.netloc:
        return
    target = (source.parent / unquote(url.path)).resolve() if url.path else source
    if target.is_dir():
        target /= 'index.html'
    if not target.is_relative_to(ROOT):
        errors.append(f'{source.name}: link escapes site root: {link}')
    elif not target.exists():
        errors.append(f'{source.name}: missing target {link}')
    elif url.fragment and target in pages and unquote(url.fragment) not in pages[target].ids:
        errors.append(f'{source.name}: missing anchor {link}')

for path, page in pages.items():
    if page.h1 != 1 or page.main != 1:
        errors.append(f'{path.name}: expected one h1 and one main')
    duplicates = [id for id, count in Counter(page.ids).items() if count > 1]
    if duplicates:
        errors.append(f'{path.name}: duplicate IDs: {duplicates}')
    for link in page.links:
        check_link(path, link)

for entry in json.loads((ROOT / 'assets/search-index.json').read_text()):
    check_link(ROOT / 'docs/getting-started.html', entry['url'])

# Syntax highlighting must never change the command copied from an example.
class ExampleText(HTMLParser):
    def __init__(self, html):
        super().__init__()
        self.in_pre = False
        self.parts = []
        self.examples = []
        self.feed(html)
    def handle_starttag(self, tag, attrs):
        if tag == 'pre':
            self.in_pre = True
            self.parts = []
    def handle_data(self, data):
        if self.in_pre:
            self.parts.append(data)
    def handle_endtag(self, tag):
        if tag == 'pre':
            self.examples.append(''.join(self.parts))
            self.in_pre = False

for source in sorted((ROOT / 'content/docs').glob('*.html')):
    generated = ROOT / 'docs' / source.name
    if ExampleText(source.read_text()).examples != ExampleText(generated.read_text()).examples:
        errors.append(f'{source.name}: generated code examples differ from their source')

# Discovery and alternate representations describe only the nine reviewed public pages.
class Metadata(HTMLParser):
    def __init__(self, html):
        super().__init__()
        self.links = []
        self.meta = {}
        self.feed(html)
    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == 'link':
            self.links.append(attrs)
        elif tag == 'meta':
            self.meta[attrs.get('name', attrs.get('property', ''))] = attrs.get('content', '')

public = [('index.html', '', 'index.md', ROOT / 'content/index.html')]
public += [(f'docs/{slug}.html', f'docs/{slug}.html', f'docs/{slug}.md', ROOT / f'content/docs/{slug}.html') for slug, _, _ in PAGES]
canonicals = set()
for html_path, canonical_path, md_path, source in public:
    html_content = (ROOT / html_path).read_text()
    metadata = Metadata(html_content)
    canonical = BASE + canonical_path
    canonicals.add(canonical)
    for relation, expected in [('canonical', canonical), ('alternate', BASE + md_path), ('describedby', BASE + 'llms.txt')]:
        matches = [link for link in metadata.links if relation in link.get('rel', '').split()]
        if len(matches) != 1 or matches[0].get('href') != expected:
            errors.append(f'{html_path}: inconsistent {relation} discovery')
        if relation == 'alternate' and matches and matches[0].get('type') != 'text/markdown':
            errors.append(f'{html_path}: Markdown alternate has wrong type')
    if metadata.meta.get('og:url') != canonical or metadata.meta.get('og:image') != BASE + 'assets/social-card.png':
        errors.append(f'{html_path}: inconsistent social URLs')
    if metadata.meta.get('twitter:card') != 'summary_large_image':
        errors.append(f'{html_path}: missing social card metadata')
    schema = re.findall(r'<script type="application/ld\+json">(.*?)</script>', html_content, re.S)
    try:
        graph = json.loads(schema[0])['@graph'] if len(schema) == 1 else []
        if not any(node.get('url') == canonical and node.get('@type') in {'WebPage', 'TechArticle'} for node in graph):
            errors.append(f'{html_path}: missing structured page identity')
    except (ValueError, KeyError, IndexError):
        errors.append(f'{html_path}: invalid structured data')
    md = (ROOT / md_path).read_text()
    if f'Canonical: {canonical}\n' not in md or BASE + md_path not in (ROOT / 'llms.txt').read_text():
        errors.append(f'{md_path}: missing canonical or llms discovery')
    if re.search(r'<(?:html|script|button|canvas|svg)\b', md, re.I):
        errors.append(f'{md_path}: contains app chrome or executable HTML')
    examples = re.findall(r'^```\n(.*?)\n```$', md, re.M | re.S)
    if examples != ExampleText(source.read_text()).examples:
        errors.append(f'{md_path}: Markdown command examples differ from their source')
    for destination in re.findall(r'\]\((https?://[^)]+)\)', md):
        if destination.startswith(BASE):
            check_link(ROOT / 'index.html', destination[len(BASE):])

sitemap = ET.fromstring((ROOT / 'sitemap.xml').read_text())
sitemap_urls = [node.text for node in sitemap.findall('.//{http://www.sitemaps.org/schemas/sitemap/0.9}loc')]
if set(sitemap_urls) != canonicals or len(sitemap_urls) != len(canonicals):
    errors.append('Sitemap must list exactly the nine canonical HTML URLs')
expected_markdown = {ROOT / entry[2] for entry in public}
actual_markdown = {ROOT / 'index.md', *list((ROOT / 'docs').glob('*.md'))}
if actual_markdown != expected_markdown:
    errors.append('Markdown outputs differ from the explicit public allowlist')
image = (ROOT / 'assets/social-card.png').read_bytes()
if image[:8] != b'\x89PNG\r\n\x1a\n' or struct.unpack('>II', image[16:24]) != (1200, 630):
    errors.append('Social preview must be a 1200 by 630 PNG')

html = (ROOT / 'index.html').read_text()
badge = r'<a class="badge" href="https://github\.com/RA1NM4KER/agent-relay/releases/tag/(v\d+\.\d+\.\d+)">\1</a>'
meta = r'<div class="meta-row">\s*<span>(v\d+\.\d+\.\d+)</span>'
if len(re.findall(badge, html)) != 1 or len(re.findall(meta, html)) != 1:
    errors.append('Release updater requires exactly one badge and one meta version marker.')
if any('gradient(' in path.read_text().lower() for path in (ROOT / 'assets').glob('*.css')):
    errors.append('Gradients are not permitted.')

# When checked out beside the CLI source, exercise the actual existing updater too.
updater = ROOT.parent / 'agent-relay/scripts/update-pages-stable-version.py'
if updater.exists():
    spec = importlib.util.spec_from_file_location('release_updater', updater)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    changed = module.update(html, 'v9.8.7')
    assert module.update(changed, 'v9.8.7') == changed
    assert 'releases/tag/v9.8.7">v9.8.7</a>' in changed
    assert '<span>v9.8.7</span>' in changed
    for invalid in ('dogfood', 'v9.8.7-dev', 'latest'):
        try:
            module.update(html, invalid)
        except module.UpdateError:
            pass
        else:
            errors.append(f'Release updater accepted {invalid}')

if errors:
    raise SystemExit('\n'.join(errors))
print(f'Validated {len(pages)} pages, every local link/anchor, search targets, release markers, Markdown parity, SEO discovery, structured data, and no gradients.')
