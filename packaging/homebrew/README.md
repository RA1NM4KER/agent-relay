# Dev/dogfood Homebrew formula

`agent-relay-dev.rb` is generated, not committed here (see `.gitignore`) — it goes stale the
moment a newer dogfood build lands, so keeping a checked-in copy would just be a source of drift.

To pick up the latest green `main` build as an installable, upgradeable Homebrew formula:

```sh
scripts/render-dev-tap-formula.sh
```

This reads the real, already-published rolling `dogfood` GitHub pre-release (see
`.github/workflows/dogfood.yml`) and makes no remote changes. It is useful to inspect a formula
locally; normal dogfood publication renders and commits only `Formula/agent-relay-dev.rb` to the
tap automatically after each green `main` build. Stable `Formula/agent-relay.rb` remains manual.

Once published there, the intended dev workflow is:

```sh
brew install RA1NM4KER/tap/agent-relay-dev   # first install
brew upgrade agent-relay-dev                 # pick up a newer dogfood build later
relay-dev setup                              # align integrations to relay-dev
```

`relay-dev` and the stable `relay` (from `agent-relay`) install under different binary names and
coexist — installing or upgrading one never touches the other.
