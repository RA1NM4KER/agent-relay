# Stable maintenance releases

Stable Homebrew serves tagged releases. `main`'s green dogfood builds use the separate
`agent-relay-dev` formula and `relay-dev` binary. Do not publish a dev artifact as stable or
manually change the dev formula during a stable release.

1. Review the full delta from the preceding stable tag, current open issues, and README/help.
   Resolve safety blockers; document remaining non-blockers without closing unfinished work.
2. Set `[workspace.package].version` in `Cargo.toml`, update workspace entries in `Cargo.lock`,
   and write a dated changelog section. `build.rs` makes subsequent development builds use the
   next patch version automatically; do not bump the base again merely to obtain a dev version.
3. Run formatting, workspace Clippy with warnings denied, workspace tests with `--no-fail-fast`,
   and the release/tap guard scripts. Build with
   `RELAY_RELEASE_BUILD=1 cargo build --release --bin relay --bin relay-herdr-plugin` and verify
   the clean `relay X.Y.Z` version. Check a normal build still reports a development version.
4. Commit and push `main`. Wait for macOS and Linux CI to succeed on that exact HEAD. Preserve
   local user files and inspect tracked/untracked changes before tagging.
5. Run `python3 scripts/check-release-version.py vX.Y.Z`, verify HEAD, and push the stable tag.
   The Release workflow repeats quality checks, verifies the tag/version, builds both macOS
   architectures, and creates a **draft** GitHub release. It does not publish or update stable
   Homebrew automatically.
6. Download the draft's two tarballs, individual `.sha256` files, and `SHA256SUMS.txt`. Verify
   both architecture names and all checksums. Extract the native archive in a temporary directory
   and verify its binary reports exactly `relay X.Y.Z`. Publish only after the workflow succeeds
   and these checks pass, using the reviewed changelog section as release notes.
7. Update only `Formula/agent-relay.rb` in `RA1NM4KER/homebrew-tap`: use the real published
   artifact URLs and verified SHA-256 for each architecture, then commit and push the tap.
   Dogfood publication owns `Formula/agent-relay-dev.rb`; preserve it.
8. Run `brew update`, `brew upgrade agent-relay`, and verify the installed version. Check help,
   project status, doctor, and `relay update --check`. Authentication failures must remain clear,
   safe diagnoses; never log into or out of a profile as part of release validation.
9. Verify the published assets, tap version, CI, and tracked working tree one last time. The
   `pages-stable-version` workflow updates the Pages stable-version badge after publication.
   Confirm that workflow rather than manually changing unrelated website content.

The release workflow publishes macOS arm64 and x86_64 artifacts. Linux is built/tested by CI but
is not claimed as live-supported. Provider versions outside the live-verified set remain
unverified; a release is not permission to broaden their safety/capability gates.
