#!/usr/bin/env python3
"""Build the dependency-free Pages site. Run --check to detect stale generated files."""
from pathlib import Path
from html import escape, unescape
from html.parser import HTMLParser
import argparse
import json
import re

ROOT = Path(__file__).resolve().parent.parent
REPO = 'https://github.com/RA1NM4KER/agent-relay'
BASE = 'https://ra1nm4ker.github.io/agent-relay/'
PAGES = [
 ('getting-started', 'Get started', 'Install Relay, set up profiles, and start your first managed conversation.'),
 ('workflows', 'Everyday workflows', 'Start, resume, switch, and adopt coding-agent conversations.'),
 ('commands', 'Command reference', 'Find Agent Relay commands, options, and copyable examples.'),
 ('profiles', 'Profiles & accounts', 'Manage isolated accounts and choose your fallback order.'),
 ('handoffs', 'Handoffs & capabilities', 'Understand automatic handoff, session continuity, and current limits.'),
 ('integrations', 'Inside your agent', 'Use Relay inside Claude Code, Codex, and optional Herdr sessions.'),
 ('troubleshooting', 'Troubleshooting', 'Diagnose readiness, unavailable profiles, and interrupted handoffs.'),
 ('advanced', 'Advanced use', 'Inspect transactions, script Relay, and work with development builds.'),
]

class Text(HTMLParser):
    def __init__(self):
        super().__init__(); self.parts = []
    def handle_data(self, text): self.parts.append(text)

def plain(html):
    p = Text(); p.feed(html); return ' '.join(' '.join(p.parts).split())

def code_blocks(content):
    return re.sub(r'<pre><code>(.*?)</code></pre>', lambda m: '<div class="code-block"><pre tabindex="0"><code>' + m[1] + '</code></pre><button class="copy-button" type="button" aria-label="Copy command">Copy</button></div>', content, flags=re.S)

def header(prefix, home=False):
    return f'''<a class="skip-link" href="#main">Skip to content</a>
<header class="site-header"><div class="header-inner">
<a class="brand" href="{prefix}index.html" aria-label="Agent Relay home"><span class="brand-mark" aria-hidden="true">↳</span>Agent Relay<span class="brand-label">CLI</span></a>
<nav class="desktop-nav" aria-label="Main navigation"><a {'aria-current="page"' if home else ''} href="{prefix}index.html">Overview</a><a {'aria-current="true"' if not home else ''} href="{prefix}docs/getting-started.html">Documentation</a><a href="{REPO}">GitHub <span aria-hidden="true">↗</span></a></nav>
<div class="header-actions"><button class="theme-button" type="button" aria-label="Switch colour theme" title="Switch colour theme" hidden>◐</button><a class="button button-small" href="{prefix}docs/getting-started.html">Get started <span aria-hidden="true">↗</span></a></div>
</div></header>'''

def footer(prefix):
    return f'''<footer class="site-footer"><div><a class="brand" href="{prefix}index.html"><span class="brand-mark" aria-hidden="true">↳</span>Agent Relay</a><p>Your agents. Your accounts. Your machine.</p></div><div class="footer-links"><a href="{prefix}docs/getting-started.html">Documentation</a><a href="{REPO}">Source code ↗</a><a href="{REPO}/releases">Releases ↗</a><a href="{REPO}/blob/main/LICENSE">Apache-2.0 ↗</a></div></footer><div class="sr-only" id="copy-status" role="status" aria-live="polite"></div>'''

def shell(title, description, path, body, prefix):
    return f'''<!doctype html>
<html lang="en" data-theme="dark"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{escape(title)} · Agent Relay</title><meta name="description" content="{escape(description, quote=True)}">
<meta name="theme-color" content="#111311"><link rel="canonical" href="{BASE}{path}">
<meta property="og:type" content="website"><meta property="og:title" content="{escape(title, quote=True)} · Agent Relay"><meta property="og:description" content="{escape(description, quote=True)}"><meta property="og:url" content="{BASE}{path}">
<link rel="icon" href="{prefix}assets/favicon.svg" type="image/svg+xml"><link rel="stylesheet" href="{prefix}assets/site.css"><script src="{prefix}assets/theme.js"></script><script src="{prefix}assets/site.js" defer></script>
</head><body>{body}</body></html>\n'''

def build():
    # The existing release workflow updates these two markers in index.html.
    # Read its current badge so a docs rebuild never rolls the published version back.
    current = (ROOT / 'index.html').read_text() if (ROOT / 'index.html').exists() else ''
    version = re.search(r'<a class="badge" href="https://github.com/RA1NM4KER/agent-relay/releases/tag/(v\d+\.\d+\.\d+)">', current)
    version = version[1] if version else 'v0.4.1'
    outputs = {}
    content = (ROOT / 'content/index.html').read_text().replace('{{VERSION}}', version)
    outputs['index.html'] = shell('Keep your coding work moving', 'Continue coding across Claude and Codex profiles. Local supervision, automatic handoff, and practical documentation.', '', header('', True) + code_blocks(content) + footer(''), '')
    search = []
    for n, (slug, title, desc) in enumerate(PAGES):
        content = code_blocks((ROOT / f'content/docs/{slug}.html').read_text())
        headings = re.findall(r'<h2 id="([^"]+)">(.*?)</h2>', content)
        nav = ''.join(f'<a {"aria-current=\"page\"" if s == slug else ""} href="{s}.html">{t}</a>' for s, t, _ in PAGES)
        sidenav = f'<span class="nav-label">Documentation</span>{nav}<div class="sidebar-meta">Examples verified against<br><a href="{REPO}/tree/v0.4.1">stable v0.4.1 ↗</a></div>'
        toc = ''.join(f'<a href="#{i}">{plain(t)}</a>' for i,t in headings)
        adjacent = '<nav class="page-pagination" aria-label="Documentation pages">'
        for index, label in [(n-1, 'Previous'), (n+1, 'Next')]:
            if 0 <= index < len(PAGES):
                s,t,_ = PAGES[index]; adjacent += f'<a href="{s}.html"><span>{label}</span>{t} {"→" if label == "Next" else ""}</a>'
        adjacent += '</nav>'
        search_ui = '<div class="search-area"><label for="docs-search">Search the docs</label><div class="search-input-wrap"><span aria-hidden="true">⌕</span><input id="docs-search" type="search" placeholder="Commands, questions, workflows…" autocomplete="off" aria-controls="search-results"><kbd>/</kbd></div><p id="search-status" role="status"></p><div id="search-results" hidden></div><noscript><p>Browse the navigation or use your browser’s Find command.</p></noscript></div>'
        body = header('../') + f'<div class="docs-layout"><aside class="docs-sidebar" aria-label="Documentation navigation"><nav>{sidenav}</nav></aside><main class="doc-main" id="main"><details class="mobile-doc-nav"><summary>Browse documentation</summary><nav>{sidenav}</nav></details>{search_ui}<div class="doc-eyebrow">The Relay handbook</div><h1>{title}</h1><p class="doc-lead">{desc}</p><div class="doc-content">{content}</div>{adjacent}</main><aside class="page-toc"><span class="nav-label">On this page</span><nav aria-label="On this page">{toc}</nav><a class="source-link" href="{REPO}/blob/v0.4.1/crates/relay-cli/src/cli.rs">CLI source ↗</a></aside></div>' + footer('../')
        outputs[f'docs/{slug}.html'] = shell(title, desc, f'docs/{slug}.html', body, '../')
        sections = re.split(r'(?=<h2 id=")', content)
        search.append({'title':title, 'url':f'{slug}.html', 'text':desc})
        for section in sections:
            h = re.match(r'<h2 id="([^"]+)">(.*?)</h2>', section)
            if h: search.append({'title':plain(h[2]), 'page':title, 'url':f'{slug}.html#{h[1]}', 'text':plain(section)})
    outputs['assets/search-index.json'] = json.dumps(search, ensure_ascii=False, indent=2) + '\n'
    outputs['sitemap.xml'] = '<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' + ''.join(f'<url><loc>{BASE}{p}</loc></url>' for p in outputs if p.endswith('.html')) + '</urlset>\n'
    return outputs

if __name__ == '__main__':
    parser = argparse.ArgumentParser(); parser.add_argument('--check', action='store_true'); args = parser.parse_args()
    stale = []
    for name, data in build().items():
        path = ROOT / name
        if args.check:
            if not path.exists() or path.read_text() != data: stale.append(name)
        else:
            path.parent.mkdir(parents=True, exist_ok=True); path.write_text(data)
    if stale: raise SystemExit('Out of date: ' + ', '.join(stale))
    print('Generated files are current.' if args.check else 'Built homepage, 8 docs pages, search index, and sitemap.')
