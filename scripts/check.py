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

html = (ROOT / 'index.html').read_text()
badge = r'<a class="badge" href="https://github\.com/RA1NM4KER/agent-relay/releases/tag/(v\d+\.\d+\.\d+)">\1</a>'
meta = r'<div class="meta-row">\s*<span>(v\d+\.\d+\.\d+)</span>'
if len(re.findall(badge, html)) != 1 or len(re.findall(meta, html)) != 1:
    errors.append('Release updater requires exactly one badge and one meta version marker.')
if 'gradient(' in (ROOT / 'assets/site.css').read_text().lower():
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
print(f'Validated {len(pages)} pages, every local link/anchor, search targets, release markers, and no gradients.')
