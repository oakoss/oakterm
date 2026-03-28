---
title: 'Distribution'
status: draft
category: cross-cutting
description: 'Release channels, packaging, crate publishing, OIDC trusted publishing'
tags: ['distribution', 'releases', 'homebrew', 'crates-io', 'ci']
---

# Distribution

How OakTerm gets from source to users. Three channels: GitHub Releases for compiled binaries, Homebrew for macOS, crates.io for source builds. Each channel maps to an `INSTALL_SOURCE` value from [ADR 0003](../adrs/0003-update-check-policy.md).

## Distribution Channels

| Channel         | What ships                                | `INSTALL_SOURCE` | Update path         |
| --------------- | ----------------------------------------- | ---------------- | ------------------- |
| GitHub Releases | Compiled binaries, macOS .app, .deb, .msi | `github-release` | In-app update check |
| Homebrew        | Formula in `oakoss/homebrew-tap`          | `homebrew`       | `brew upgrade`      |
| crates.io       | Source crate, `cargo install oakterm`     | `source`         | `cargo install`     |

Windows packaging (winget, scoop) follows when Windows support ships.

## GitHub Releases

Compiled binaries for each platform, attached to tagged releases.

### Targets

| Platform       | Target triple              | Artifact                           |
| -------------- | -------------------------- | ---------------------------------- |
| macOS ARM      | `aarch64-apple-darwin`     | `oakterm-macos-arm64.tar.gz`, .app |
| macOS Intel    | `x86_64-apple-darwin`      | `oakterm-macos-x86_64.tar.gz`      |
| Linux x86_64   | `x86_64-unknown-linux-gnu` | `oakterm-linux-x86_64.tar.gz`      |
| Windows x86_64 | `x86_64-pc-windows-msvc`   | `oakterm-windows-x86_64.zip`       |

### CI Release Workflow

Triggered by pushing a version tag (`v0.1.0`, `v0.2.0-nightly.2026.03.28`):

1. Build binaries for all targets (cross-compile or per-platform runners)
2. Run full test suite on each platform
3. Create GitHub Release with changelog from conventional commits
4. Attach compiled artifacts
5. Publish to crates.io via OIDC (see below)
6. Update Homebrew formula

Build environment sets `INSTALL_SOURCE` and `RELEASE_CHANNEL` per [ADR 0003](../adrs/0003-update-check-policy.md):

```bash
INSTALL_SOURCE=github-release RELEASE_CHANNEL=stable cargo build --release
```

## Homebrew

A tap at `oakoss/homebrew-tap` with a formula that builds from source or installs a prebuilt bottle.

```bash
brew tap oakoss/tap
brew install oakterm
```

The formula sets `INSTALL_SOURCE=homebrew` during build. Update checks are disabled for Homebrew installs — `brew upgrade` is the update path.

## crates.io

All workspace crates published to crates.io. Users can install from source:

```bash
cargo install oakterm
```

### Publishing Order

Dependencies publish before dependents:

1. `oakterm-common`
2. `oakterm-protocol`, `oakterm-pty`, `oakterm-config`, `oakterm-a11y`
3. `oakterm-terminal`, `oakterm-renderer`
4. `oakterm-daemon`, `oakterm-ctl`
5. `oakterm`

### Ownership

- Initial publish under `jbabin91` account
- Add team: `cargo owner --add github:oakoss:core oakterm` (repeat per crate)
- Team members can publish and yank; only individual owners manage other owners

### Namespace

crates.io has no org namespaces yet. RFC 3243 ("Packages as Optional Namespaces") was accepted but implementation is incomplete. We use the `oakterm-*` prefix convention. If `oakterm::*` namespacing ships, we migrate.

## OIDC Trusted Publishing

crates.io supports trusted publishing via GitHub Actions OIDC tokens (since July 2025). No long-lived API tokens to store or rotate.

### Setup

1. First publish of each crate is manual (`cargo publish` with API token)
2. Configure trusted publisher on crates.io per crate — link to `oakoss/oakterm` repo and workflow filename
3. Enable "Trusted Publishing Only" mode to block API token publishing

### Workflow

```yaml
jobs:
  publish:
    runs-on: ubuntu-latest
    environment: release
    permissions:
      id-token: write
    steps:
      - uses: actions/checkout@v6
      - uses: rust-lang/crates-io-auth-action@v1
        id: auth
      - run: cargo publish
        env:
          CARGO_REGISTRY_TOKEN: ${{ steps.auth.outputs.token }}
```

### Constraints

- OIDC tokens are short-lived (30 minutes)
- `pull_request_target` and `workflow_run` triggers are blocked for security
- GitLab CI also supported; other providers not yet

## What This Is Not

- Not a package manager — we distribute through existing ones
- Not a self-update mechanism — that's [Updates](24-updates.md) and [ADR 0003](../adrs/0003-update-check-policy.md)
- Not a plugin registry — plugin distribution is a separate concern ([Plugins](06-plugins.md))

## Related Docs

- [Updates](24-updates.md) — update notification and installation UX
- [ADR 0003](../adrs/0003-update-check-policy.md) — install-source-aware update checks
- [Roadmap](33-roadmap.md) — release timing is end of Phase 0
