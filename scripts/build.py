#!/usr/bin/env python3
"""Build the dependency-free Pages site. Run --check to detect stale generated files."""
from pathlib import Path
from html import escape, unescape
from html.parser import HTMLParser
import argparse
import hashlib
import json
import re
from public_content import markdown

ROOT = Path(__file__).resolve().parent.parent
REPO = 'https://github.com/RA1NM4KER/agent-relay'
BASE = 'https://ra1nm4ker.github.io/agent-relay/'
HANDBOOK_VERSION = 'v0.5.2'
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

def command_colors(code):
    lines = []
    for line in unescape(code).split('\n'):
        if line.lstrip().startswith('#'):
            lines.append('<span class="syntax-comment">' + escape(line) + '</span>')
            continue
        command = re.match(r'^(\s*)(relay|brew|git|cd|cargo|claude|codex|curl|export|mkdir|cp|chmod|python3|npm|npx)(?=\s|$)', line)
        if not command:
            lines.append(escape(line))
            continue
        result = escape(command[1]) + '<span class="syntax-command">' + command[2] + '</span>'
        rest = line[command.end():]
        cursor = 0
        for token in re.finditer(r'''"(?:\\.|[^"\\])*"|'[^']*'|--[\w-]+|(?<!\S)#[^\n]*''', rest):
            result += escape(rest[cursor:token.start()])
            value = token[0]
            kind = 'syntax-flag' if value.startswith('--') else 'syntax-comment' if value.startswith('#') else 'syntax-string'
            result += f'<span class="{kind}">' + escape(value) + '</span>'
            cursor = token.end()
        lines.append(result + escape(rest[cursor:]))
    return '\n'.join(lines)

def code_blocks(content, highlight=False):
    return re.sub(r'<pre><code>(.*?)</code></pre>', lambda m: '<div class="code-block"><pre tabindex="0"><code>' + (command_colors(m[1]) if highlight else m[1]) + '</code></pre><button class="copy-button" type="button" aria-label="Copy command">Copy</button></div>', content, flags=re.S)

def header(prefix, home=False):
    current_home = ' aria-current="page"' if home else ''
    current_docs = ' aria-current="true"' if not home else ''
    return f'''<a class="skip-link" href="#main">Skip to content</a>
<header class="site-header"><div class="header-inner">
<a class="brand" href="{prefix or './'}" aria-label="Agent Relay home"{current_home}><span class="brand-mark" aria-hidden="true">↳</span>Agent Relay</a>
<nav class="front-nav" aria-label="Main navigation"><a href="{prefix}docs/getting-started.html"{current_docs}>Documentation</a><a href="{REPO}">GitHub <span aria-hidden="true">↗</span></a></nav>
<button class="theme-button" type="button" aria-label="Switch colour theme" title="Switch colour theme" hidden>◐</button>
</div></header>'''

def footer(prefix, version=None):
    if version:
        return f'''<footer class="site-footer front-footer"><div><a class="brand" href="./">Agent Relay</a><div class="meta-row"><span>{version}</span><span>Apache 2.0</span></div></div><div class="footer-links"><a class="badge" href="{REPO}/releases/tag/{version}">{version}</a><a href="{REPO}/releases">Releases</a><a href="{REPO}/blob/main/LICENSE">License</a><a href="{REPO}">Source ↗</a></div></footer><div class="sr-only" id="copy-status" role="status" aria-live="polite"></div>'''
    return f'''<footer class="site-footer"><div><a class="brand" href="{prefix or './'}">Agent Relay</a><p>Local tools. Clear ownership.</p></div><div class="footer-links"><a href="{prefix or './'}">Overview</a><a href="{REPO}/releases">Releases</a><a href="{REPO}/blob/main/LICENSE">License</a><a href="{REPO}">Source ↗</a></div></footer><div class="sr-only" id="copy-status" role="status" aria-live="polite"></div>'''

def structured_data(title, description, path):
    canonical = BASE + path
    page = {
        '@type': 'WebPage' if not path else 'TechArticle',
        '@id': canonical + '#page', 'url': canonical, 'name': title + ' · Agent Relay',
        'description': description, 'inLanguage': 'en',
        'isPartOf': {'@id': BASE + '#website'},
        'about': {'@id': BASE + '#software'},
    }
    graph = [{'@type': 'WebSite', '@id': BASE + '#website', 'url': BASE, 'name': 'Agent Relay', 'inLanguage': 'en'}, page]
    if path:
        page['version'] = HANDBOOK_VERSION
        page['breadcrumb'] = {'@id': canonical + '#breadcrumbs'}
        graph.append({'@type': 'BreadcrumbList', '@id': canonical + '#breadcrumbs', 'itemListElement': [
            {'@type': 'ListItem', 'position': 1, 'name': 'Agent Relay', 'item': BASE},
            {'@type': 'ListItem', 'position': 2, 'name': title, 'item': canonical},
        ]})
    else:
        page['mainEntity'] = {'@id': BASE + '#software'}
        graph.append({'@type': 'SoftwareApplication', '@id': BASE + '#software', 'name': 'Agent Relay',
            'url': BASE, 'description': description, 'applicationCategory': 'DeveloperApplication',
            'operatingSystem': 'macOS', 'license': 'https://www.apache.org/licenses/LICENSE-2.0',
            'installUrl': BASE + 'docs/getting-started.html',
            'sameAs': REPO})
    return json.dumps({'@context': 'https://schema.org', '@graph': graph}, ensure_ascii=False).replace('<', r'\u003c')


def public_markdown(title, description, path, content):
    note = f'Handbook examples are verified against stable {HANDBOOK_VERSION}.'
    return f'# {title}\n\nCanonical: {BASE}{path}\n\n{description}\n\n{note}\n\n' + markdown(content, BASE + path)

def shell(title, description, path, body, prefix):
    def versioned_asset(name):
        digest = hashlib.sha256((ROOT / 'assets' / name).read_bytes()).hexdigest()[:12]
        return f'{prefix}assets/{name}?v={digest}'
    page_assets = f'<link rel="stylesheet" href="{versioned_asset("brand.css")}">'
    if path == '':
        page_assets += f'<link rel="stylesheet" href="{versioned_asset("home.css")}"><script src="{versioned_asset("home.js")}" defer></script><script src="{versioned_asset("relay-particles.js")}" defer></script>'
    else:
        page_assets += f'<link rel="stylesheet" href="{versioned_asset("docs.css")}"><script src="{versioned_asset("docs.js")}" defer></script>'
    body_class = ' class="relay-home"' if path == '' else ' class="relay-docs"'
    markdown_path = path.removesuffix('.html') + '.md' if path else 'index.md'
    social_type = 'article' if path else 'website'
    schema = structured_data(title, description, path)
    return f'''<!doctype html>
<html lang="en" data-theme="dark"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{escape(title)} · Agent Relay</title><meta name="description" content="{escape(description, quote=True)}">
<meta name="theme-color" content="#0b0b0c"><link rel="canonical" href="{BASE}{path}">
<meta property="og:type" content="{social_type}"><meta property="og:site_name" content="Agent Relay"><meta property="og:title" content="{escape(title, quote=True)} · Agent Relay"><meta property="og:description" content="{escape(description, quote=True)}"><meta property="og:url" content="{BASE}{path}">
<meta property="og:image" content="{BASE}assets/social-card.png"><meta property="og:image:width" content="1200"><meta property="og:image:height" content="630"><meta property="og:image:alt" content="Agent Relay: local handoffs for Claude Code and Codex">
<meta name="twitter:card" content="summary_large_image"><meta name="twitter:title" content="{escape(title, quote=True)} · Agent Relay"><meta name="twitter:description" content="{escape(description, quote=True)}"><meta name="twitter:image" content="{BASE}assets/social-card.png"><meta name="twitter:image:alt" content="Agent Relay: local handoffs for Claude Code and Codex">
<link rel="alternate" type="text/markdown" href="{BASE}{markdown_path}"><link rel="describedby" type="text/plain" href="{BASE}llms.txt"><link rel="sitemap" type="application/xml" href="{BASE}sitemap.xml">
<script type="application/ld+json">{schema}</script>
<link rel="icon" href="{prefix}assets/favicon.svg" type="image/svg+xml"><link rel="stylesheet" href="{versioned_asset("site.css")}">{page_assets}<script src="{prefix}assets/theme.js"></script><script src="{versioned_asset("site.js")}" defer></script>
</head><body{body_class}>{body}</body></html>\n'''

def build():
    # The existing release workflow updates these two markers in index.html.
    # Read its current badge so a docs rebuild never rolls the published version back.
    current = (ROOT / 'index.html').read_text() if (ROOT / 'index.html').exists() else ''
    version = re.search(r'<a class="badge" href="https://github.com/RA1NM4KER/agent-relay/releases/tag/(v\d+\.\d+\.\d+)">', current)
    version = version[1] if version else 'v0.4.1'
    outputs = {}
    content = (ROOT / 'content/index.html').read_text().replace('{{VERSION}}', version)
    outputs['index.html'] = shell('Local handoffs for Claude Code and Codex', 'A local supervisor for Claude Code and Codex. Verified usage exhaustion, isolated profiles, and one active owner per Relay session.', '', header('', True) + code_blocks(content) + footer('', version), '')
    outputs['index.md'] = public_markdown('Agent Relay', 'A local supervisor for Claude Code and Codex. Verified usage exhaustion, isolated profiles, and one active owner per Relay session.', '', content)
    search = []
    for n, (slug, title, desc) in enumerate(PAGES):
        source = (ROOT / f'content/docs/{slug}.html').read_text()
        content = code_blocks(source, highlight=True)
        outputs[f'docs/{slug}.md'] = public_markdown(title, desc, f'docs/{slug}.html', source)
        headings = re.findall(r'<h2 id="([^"]+)">(.*?)</h2>', content)
        nav = ''.join(f'<a {"aria-current=\"page\"" if s == slug else ""} href="{s}.html">{t}</a>' for s, t, _ in PAGES)
        sidenav = f'<a class="handbook-home" href="../">← Back to overview</a><span class="nav-label">The handbook</span>{nav}<div class="sidebar-meta">Examples verified against<br><a href="{REPO}/tree/{HANDBOOK_VERSION}">stable {HANDBOOK_VERSION} ↗</a></div>'
        toc = ''.join(f'<a href="#{i}">{plain(t)}</a>' for i,t in headings)
        adjacent = '<nav class="page-pagination" aria-label="Documentation pages">'
        for index, label in [(n-1, 'Previous'), (n+1, 'Next')]:
            if 0 <= index < len(PAGES):
                s,t,_ = PAGES[index]; adjacent += f'<a href="{s}.html"><span>{label}</span>{t} {"→" if label == "Next" else ""}</a>'
        adjacent += '</nav>'
        search_ui = '<div class="search-area"><label for="docs-search">Search the docs</label><div class="search-input-wrap"><span aria-hidden="true">⌕</span><input id="docs-search" type="search" placeholder="Commands, questions, workflows…" autocomplete="off" aria-controls="search-results"><kbd>/</kbd></div><p id="search-status" role="status"></p><div id="search-results" hidden></div><noscript><p>Browse the navigation or use your browser’s Find command.</p></noscript></div>'
        body = header('../') + f'<div class="docs-layout"><aside class="docs-sidebar" aria-label="Documentation navigation"><nav>{sidenav}</nav></aside><main class="doc-main" id="main" tabindex="-1"><details class="mobile-doc-nav"><summary>Browse documentation</summary><nav>{sidenav}</nav></details>{search_ui}<div class="doc-eyebrow">The Relay handbook <a href="{REPO}/tree/{HANDBOOK_VERSION}" aria-label="Handbook verified against {HANDBOOK_VERSION}">{HANDBOOK_VERSION}</a></div><h1>{title}</h1><p class="doc-lead">{desc}</p><details class="mobile-page-toc"><summary>On this page</summary><nav aria-label="Page sections">{toc}</nav></details><div class="doc-content">{content}</div>{adjacent}</main><aside class="page-toc"><span class="nav-label">On this page</span><nav aria-label="On this page">{toc}</nav><a class="source-link" href="{REPO}/blob/{HANDBOOK_VERSION}/crates/relay-cli/src/cli.rs">CLI source ↗</a></aside></div>' + footer('../')
        outputs[f'docs/{slug}.html'] = shell(title, desc, f'docs/{slug}.html', body, '../')
        sections = re.split(r'(?=<h2 id=")', content)
        search.append({'title':title, 'url':f'{slug}.html', 'text':desc})
        for section in sections:
            h = re.match(r'<h2 id="([^"]+)">(.*?)</h2>', section)
            if h: search.append({'title':plain(h[2]), 'page':title, 'url':f'{slug}.html#{h[1]}', 'text':plain(section)})
    outputs['assets/search-index.json'] = json.dumps(search, ensure_ascii=False, indent=2) + '\n'
    outputs['sitemap.xml'] = '<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' + ''.join(f'<url><loc>{BASE}{"" if p == "index.html" else p}</loc></url>' for p in outputs if p.endswith('.html')) + '</urlset>\n'
    outputs['llms.txt'] = '# Agent Relay\n\n> A local supervisor for Claude Code and Codex conversations.\n\n' + \
        f'Canonical website: {BASE}\n\nHandbook examples are verified against stable {HANDBOOK_VERSION}. Read the handoff guide for continuity limits and the security model for ownership and authentication boundaries.\n\n## Public pages\n\n' + \
        f'- [Product overview]({BASE}index.md): What Relay does, installation, and common questions.\n' + \
        ''.join(f'- [{title}]({BASE}docs/{slug}.md): {desc}\n' for slug, title, desc in PAGES) + \
        f'\n## Source and releases\n\n- [Source repository]({REPO}): Code, license, and project history.\n- [Stable handbook source]({REPO}/tree/{HANDBOOK_VERSION}): Version used to verify documentation examples.\n- [Releases]({REPO}/releases): Release notes and downloads.\n'
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
    print('Generated files are current.' if args.check else 'Built 9 public pages, Markdown alternates, llms.txt, search index, and sitemap.')
