# Agent Relay website

The public product introduction and CLI handbook at
[ra1nm4ker.github.io/agent-relay](https://ra1nm4ker.github.io/agent-relay/).
GitHub Pages serves the committed static files from the root of `gh-pages`.
There is no application server, runtime framework, analytics, or CDN dependency.
The site serves Geist locally from `assets/fonts/`, with its SIL Open Font License alongside it.

## Edit and build

Requires Python 3.12 or newer. Edit `content/index.html` or `content/docs/*.html`,
then run:

```sh
python3 scripts/build.py
python3 scripts/build.py --check
python3 scripts/check.py
```

The builder owns `index.html`, `docs/*.html`, `assets/search-index.json`, and
`sitemap.xml`, `index.md`, `docs/*.md`, and `llms.txt`. Commit generated output alongside its source. Shared layout lives in
`scripts/build.py`; styles and browser behaviour live in `assets/`. Shared identity and fonts live in `assets/brand.css`; the homepage uses `assets/home.css`
and the handbook uses `assets/docs.css`. `assets/site.css` keeps the base primitives. Styles use solid
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
GitHub Pages project-path layout. It checks all nine pages at 1440, 1024, 768, 390, and 320
pixels, dark/light WCAG A/AA rules with axe, search and failure recovery, copy and denied
clipboard behaviour, keyboard navigation, theme persistence, and no-JavaScript navigation.
Screenshots are written to ignored `artifacts/`. Automated accessibility checks complement,
but do not replace, manual keyboard and visual review.

The Python check verifies every local link and fragment, unique IDs, page landmarks,
search destinations, and release badge markers. If the CLI source checkout is a sibling
named `agent-relay`, it additionally exercises the actual stable-version updater in memory.
The `Site checks` workflow runs on pull requests and pushes targeting `gh-pages`.

## Version policy

The handbook is explicitly verified against **v0.5.2**, using that tag's CLI declarations
and implementation, README, and behaviour documentation. Links to technical details are
pinned to that tag. Development-only features are marked separately; do not silently mix
`main` behaviour into stable examples. Status defaults to local state; `--live` requests fresh
provider checks. The stable reference includes `refresh`, `mode`, `state`, and the
repository-specific `task` family, with its remote effects described explicitly.

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

The public `#top`, `#how-it-works`, `#security`, and `#handoff-demo` anchors remain
available. The homepage cycles through representative v0.5.2 status, doctor, and profile-list excerpts
with fictional profiles and session ID. A complete static status excerpt remains readable
without JavaScript and with reduced motion. Commands, profile names, and successful checks
use distinct colors. Each command types character by character, followed by a short beat
and a smooth line-by-line response. Successful checks and the executable use green,
profile names use blue, flags use amber, and paths stay muted. Playback continues while hovered or focused, and holds only offscreen or in a hidden tab.
A single output element serves both playback and the accessible static fallback, so copied
text never contains a second hidden transcript. Homepage asset URLs include content hashes
to pick up script and stylesheet changes when refreshing a preview. Installation is near the top; conversation-continuity details sit in native
expandable questions. The homepage and handbook share the same header and brand treatment; the release badge and
meta-row markers live in the footer so release updates retain their existing contract.

## Homepage art and motion

The homepage uses a neutral charcoal and amber palette, distinct section surfaces, and a
locally served transparent sculpture at `assets/relay-sculpture-v1.png`. It was generated
with the built-in image tool from this direction: two interlocked capsule links, amber
anodized metal and smoked graphite glass, realistic studio lighting, transparent background,
no text, no green. The asset is decorative and conveys connection rather than provider state.
The hero uses a local canvas particle sculpture: two linked forms gather, turn, and flow
into a continuous loop. Pointer movement gently displaces nearby particles. It stops rendering
offscreen or in a hidden tab. The generated studio render is the fallback for reduced motion
and no JavaScript. There are no visible pause controls. Scroll entrances run once and all content stays
available without JavaScript. The handbook shares the charcoal and amber palette, Geist typography, and header. It uses
a reading layout with an active section rail, native mobile navigation, syntax-colored
copyable examples, and quieter callouts and tables. Reference text remains static.

## Public discovery and SEO

The homepage and the eight handbook pages are the explicit public content allowlist.
`scripts/public_content.py` converts their source fragments to Markdown during the normal
build, preserving command examples, continuation limits, platform limitations, and absolute
source links. No filesystem-wide export or private/authenticated mirror is created.
HTML advertises its own Markdown alternate and the project-scoped `llms.txt`.

The sitemap includes only canonical HTML URLs, using the project root for the homepage.
JSON-LD describes the website and software on the homepage, and versioned technical articles
and breadcrumbs in the handbook. Titles, descriptions, Open Graph, and social card metadata
are emitted by the shared shell. Regenerate `assets/social-card.png` after changing its brand
assets with `node scripts/social-card.mjs`; its dimensions are 1200 by 630.

GitHub Pages serves this project beneath `/agent-relay/`. Root-domain robots policy is outside
this project's path; this change does not create or change crawler permissions. Markdown
HTTP MIME types and canonical Link headers depend on the host. Each Markdown body identifies
its canonical HTML source even when custom response headers are unavailable. Production
URLs stay canonical during local preview; there is no new global indexing directive.

`npm run check` verifies discovery, canonical/sitemap agreement, JSON-LD page identities,
explicit mirror coverage, social image dimensions, and exact command parity across source,
HTML, and Markdown. These checks do not measure rankings, citations, real visitor performance,
or Search Console indexing. Publish and verify production responses separately.
