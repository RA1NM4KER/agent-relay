# Agent Relay website

The public product introduction and CLI handbook at
[ra1nm4ker.github.io/agent-relay](https://ra1nm4ker.github.io/agent-relay/).
GitHub Pages serves the committed static files from the root of `gh-pages`.
There is no application server, runtime framework, third-party font, analytics, or CDN dependency.

## Edit and build

Requires Python 3.12 or newer. Edit `content/index.html` or `content/docs/*.html`,
then run:

```sh
python3 scripts/build.py
python3 scripts/build.py --check
python3 scripts/check.py
```

The builder owns `index.html`, `docs/*.html`, `assets/search-index.json`, and
`sitemap.xml`. Commit generated output alongside its source. Shared layout lives in
`scripts/build.py`; styles and browser behaviour live in `assets/`. Styles use solid
colours only, with dark and light themes. All core content and navigation work without
JavaScript; search, theme persistence, and copying are progressive enhancements.

The content files are HTML fragments. Keep section headings in the form
`<h2 id="stable-anchor">Heading</h2>` and shell examples as
`<pre><code>escaped command</code></pre>`. The builder uses those forms for the table
of contents, search entries, and copy buttons. Escape `<`, `>`, and `&` in examples.

## Preview and verify

```sh
python3 -m http.server 4173 --bind 127.0.0.1
```

Open http://127.0.0.1:4173. For full browser verification (Node 22+):

```sh
npm ci
npx playwright install chromium
npm run check
npm run test:browser
```

The browser check starts its own temporary server under `/agent-relay/`, matching the
GitHub Pages project-path layout. It checks all nine pages at 1440, 768, 390, and 320
pixels, dark/light WCAG A/AA rules with axe, search and failure recovery, copy and denied
clipboard behaviour, keyboard navigation, theme persistence, and no-JavaScript navigation.
Screenshots are written to ignored `artifacts/`. Automated accessibility checks complement,
but do not replace, manual keyboard and visual review.

The Python check verifies every local link and fragment, unique IDs, page landmarks,
search destinations, and release badge markers. If the CLI source checkout is a sibling
named `agent-relay`, it additionally exercises the actual stable-version updater in memory.
The `Site checks` workflow runs on pull requests and pushes targeting `gh-pages`.

## Version policy

The handbook is explicitly verified against **v0.4.1**, using that tag's CLI declarations
and implementation, README, and behaviour documentation. Links to technical details are
pinned to that tag. Development-only features are marked separately; do not silently mix
`main` behaviour into stable examples. In particular, `status --live`, `refresh`, `mode`,
`state`, and `task` are not stable v0.4.1 commands.

The existing release workflow on `main` updates exactly two markers in `index.html`:

- `<a class="badge" href="https://github.com/RA1NM4KER/agent-relay/releases/tag/vX.Y.Z">vX.Y.Z</a>`
- `<div class="meta-row"><span>vX.Y.Z</span>…</div>`

Preserve these shapes. The builder reads the current badge from the generated homepage so
rebuilding the handbook does not revert an automated release update. The latest-release
badge and the handbook's verified version are deliberately independent: a release badge
update must not imply that documentation has been reverified. When updating the handbook,
review changes from its pinned tag, update content and source links, update the shared
sidebar's verified version in the builder, and rerun the checks.

## Publish

Build and validate on a branch based on the current `origin/gh-pages`. Publishing a reviewed
commit to `gh-pages` triggers the repository's existing Pages deployment. No changes to the
Rust source branch or to Pages settings are required. Never force-push over a concurrent
release badge update; rebase onto the latest `origin/gh-pages` and preserve its badge.

The previous public `#top`, `#how-it-works`, `#security`, and `#handoff-demo` links are
preserved. The portfolio video was intentionally omitted: its embedded gradient conflicts
with the solid-colour visual direction. The homepage handoff demonstration is labelled as illustrative, not live provider output.
It plays one four-stage Claude-to-Claude handoff and holds the result. Pause/play and replay
controls preserve a readable sequence; offscreen or hidden tabs suspend its clock. With
reduced motion, it shows the completed result and offers manual stepping after Replay.
Without JavaScript the full sequence remains readable. The dynamic demonstration never
shows two active owners and does not imply native continuity for Codex handoffs.
