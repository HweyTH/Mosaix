# Research: What structure should a professional open-source README have for a Rust workspace plus Tauri desktop app?

> **TL;DR**: There is no single convention worth adopting wholesale — [standard-readme](https://github.com/RichardLitt/standard-readme/blob/main/spec.md) is a library/package spec and **not one of the four comparable window managers follows it**; none carries its badge, none has its mandated Table of Contents, and three of four omit its required `Contributing` section body in favour of a one-line link. What the four *do* share is a stable shape: **visual first (logo/screenshot/demo), one-sentence positioning, a short key-features list, install, then everything deep is a link**. [komorebi](https://github.com/LGUG2Z/komorebi/blob/master/README.md) — the nearest peer — is the sharpest case: its `## Overview` is 250 words ending in a single paragraph that links out to install, configure, common workflows, schema reference and CLI reference on a [mkdocs site](https://github.com/LGUG2Z/komorebi/blob/master/mkdocs.yml), and its `# Installation` heading is *four lines long* and links to `installation.html`. Everything komorebi keeps in-README is what a docs site cannot host well: **contribution rules, debugging recipes, and the IPC/event-subscription protocol** — i.e. the parts a reader needs while looking at the repository. All four keep the **end-user install path in the README and push build-from-source out**: GlazeWM to [`CONTRIBUTING.md`](https://github.com/glzr-io/glazewm/blob/main/CONTRIBUTING.md) (which also carries the crate-by-crate "Codebase overview" — the GlazeWM README never lists its crates), AeroSpace to [`dev-docs/development.md`](https://github.com/nikitabobko/AeroSpace/blob/main/dev-docs/development.md), PowerToys to [`doc/devdocs`](https://github.com/microsoft/PowerToys/blob/main/README.md). On badges, the evidence is blunt: **not one of the four carries a tech-stack badge**, and AeroSpace ships exactly one badge (CI build status, inline in the `#` heading) while PowerToys ships **zero**. Mosaix's current README is therefore wrong in five specific ways: six static decorative badges and no live signal; a banner and tagline promising "Windows 11 and macOS" when `crates/mosaix-platform-macos/src/lib.rs` is one line; no screenshot or demo at all, which every comparable leads with; a 13-bullet feature list that reads as a changelog against GlazeWM's 5 and AeroSpace's 8; and **not one link** to the 48KB `ARCHITECTURE.md`, the `CONTEXT.md` glossary, or the 29 ADRs that already exist beside it.

## Findings

### The current Mosaix README, measured against the field

`README.md` is 82 lines with four sections: banner + badge row, `## Features`, `## Installation`, `## License`. Concretely wrong:

1. **The badges carry no information.** All six are static `img.shields.io/badge/...` strings naming Rust, Tauri 2, TypeScript, Windows 11, macOS and MIT ([`README.md` line 3](../../README.md)). None of the four comparables carries a tech-stack badge of any kind. There is also no CI badge — correctly, since the repository has no `.github/` directory at all, so there is no workflow to report on.
2. **The banner and the tagline overstate the platform.** The banner alt text reads "tiling window management for Windows 11 and macOS" and line 5 says "A cross-platform window tiling application for Windows 11 and macOS", while `crates/mosaix-platform-macos/src/lib.rs` is a **one-line file**. The macOS badge says "planned", contradicting the two lines above it. Compare AeroSpace, which puts its honesty in a heading — `# AeroSpace Beta` — and a `## Project status` section that opens "Public Beta ... expect breaking changes until 1.0" ([AeroSpace README](https://github.com/nikitabobko/AeroSpace/blob/main/README.md)).
3. **There is no image beyond the banner.** Every comparable leads with a working visual: komorebi a full screenshot, GlazeWM a `demo.webp` and later a keybinding cheatsheet PNG, AeroSpace an app icon plus two YouTube demo links, PowerToys a `<picture>` hero with light/dark sources.
4. **The feature list is a changelog.** Thirteen bullets, several three lines long, one (`Saved layouts`) running five lines and describing surplus-cell behaviour. GlazeWM's `### 🌟 Key features` is **five one-line bullets**; AeroSpace's `## Key features` is eight, each linking into the guide rather than explaining in place.
5. **Nothing links to the documentation that already exists.** The repo has `ARCHITECTURE.md` (1001 lines, 24 numbered sections including §16 "Repository structure" and §17 "Testing strategy"), `CONTEXT.md` (207 lines, a domain glossary), `AGENTS.md`, 29 ADRs under `docs/architecture-decisions/`, plus `docs/research/`, `docs/verification/` and `docs/agents/`. The README references none of them.
6. **The install section serves two audiences at once.** `git clone` + `cargo build --release` + `npm install` + `npm run tauri dev` is a contributor path presented as the only install path, and it ends with `cargo test --workspace` — a build instruction filed under "Installation".

Two adjacent gaps, from GitHub's own checklist: the community profile "checks to see if a project includes recommended community health files, such as README, CODE_OF_CONDUCT, LICENSE, or CONTRIBUTING" ([GitHub Docs — About community profiles](https://docs.github.com/en/communities/setting-up-your-project-for-healthy-contributions/about-community-profiles-for-public-repositories)). Mosaix has README and LICENSE; it has no `CONTRIBUTING.md`, no `CODE_OF_CONDUCT.md`, and no `.github/ISSUE_TEMPLATE`. Separately, `[workspace.package]` in `Cargo.toml` sets only `version`, `edition`, `license` and `repository` — no `description`, `keywords` or `categories`, which the Rust API Guidelines' **C-METADATA** ("Cargo.toml includes all common metadata") asks for ([Rust API Guidelines — Documentation](https://rust-lang.github.io/api-guidelines/documentation.html#cargotoml-includes-all-common-metadata-c-metadata)).

### Canonical section order, as the four actually order it

Reading the four raw sources side by side, the order that survives every one of them is: **identity → positioning sentence → visual → nav/badges → key features → install → configure/use → contribute → deeper links**. The variations are informative.

| | Lines | Leads with | Badges | Install in README? | Build-from-source in README? | Config docs in README? | Architecture in README? |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **komorebi** | 515 | H1, one-line tagline, 10-badge block, screenshot | 10 | Link only (4 lines + video) | **No** — "shows how to get started using `scoop`, `winget` or building from source" links to `installation.html` | **No** — links to `example-configurations.html` and a hosted schema reference | No — `docs/design.md` on the site |
| **GlazeWM** | 393 | `<div align="center">`, release note, logo SVG, H1, tagline, 3 badges, nav row, demo video | 3 | **Yes** — releases link + winget/choco/scoop | **No** — `CONTRIBUTING.md` | **Yes** — 7 `### Config: *` subsections, ~280 lines | **No** — `CONTRIBUTING.md` has "Codebase overview / Crates" |
| **AeroSpace** | 172 | H1 with inline CI badge, right-floated icon, one-line description, then `Videos:` and `Docs:` link lists | 1 | **Yes** — one `brew install --cask` block | **No** — `dev-docs/development.md` | **No** — links to the guide | No — the guide |
| **PowerToys** | 121 | `<picture>` hero, centred H1, one-line description, centred nav row | **0** | **Yes** — 4 collapsed `<details>` blocks | **No** — `doc/devdocs` | No — `learn.microsoft.com` | No — `doc/devdocs` |

The consistent rules underneath that table:

- **The positioning sentence is one line and appears before anything else textual.** komorebi: "Tiling Window Management for Windows." GlazeWM: "**A tiling window manager for Windows inspired by i3wm.**" AeroSpace: "AeroSpace is an i3-like tiling window manager for macOS". PowerToys: "Microsoft PowerToys is a collection of utilities that help you customize Windows and streamline everyday tasks."
- **A nav row replaces a table of contents.** GlazeWM renders `[Installation](#installation) • [Default keybindings](#default-keybindings) • [Config documentation](#config-documentation) • [FAQ](#faq) • [Contributing ↗](.../CONTRIBUTING.md)`; PowerToys renders an `<h3 align="center">` row of `Installation · Documentation · Blog · Release notes`. Neither writes a bulleted TOC. That is consistent with GitHub generating one for free: "GitHub will automatically generate a table of contents based on section headings" ([GitHub Docs — About READMEs](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-readmes)).
- **License is a section only when the license is unusual.** komorebi devotes three sections and ~400 words to it because it ships under a PolyForm Strict fork with a paid commercial tier. GlazeWM, AeroSpace and PowerToys give it **no section at all** — the sidebar's LICENSE detection is enough. Mosaix is MIT; a `## License` section of the word "MIT" is the weakest possible use of a heading.
- **Contributing is a link, not a section body — with one exception.** GlazeWM: three sentences and a link. AeroSpace: routes to Discussions and `CONTRIBUTING.md`. PowerToys: one paragraph and a link. komorebi is the outlier at ~400 words across four `##` subsections (`Commit hygiene`, `PRs should contain only a single feature or bug fix`, `Refactors to the codebase must have prior approval`, `Breaking changes to user-facing interfaces are unacceptable`) — and it *still* links to `CONTRIBUTING.md` for the licensing half. The rule is: rules a maintainer will enforce on your first PR go in the README; setup steps go in `CONTRIBUTING.md`.

### The README-vs-linked-docs line, and how komorebi draws it

komorebi is the nearest peer and the clearest test, because it has the same problem Mosaix has — a large body of existing documentation — and it solved it by moving nearly everything out.

The whole of komorebi's user documentation is deferred in one paragraph:

> "Please refer to the [documentation](https://lgug2z.github.io/komorebi) for instructions on how to [install](https://lgug2z.github.io/komorebi/installation.html) and [configure](https://lgug2z.github.io/komorebi/example-configurations.html) _komorebi_, [common workflows](https://lgug2z.github.io/komorebi/common-workflows/komorebi-config-home.html), a complete [configuration schema reference](...) and a complete [CLI reference](https://lgug2z.github.io/komorebi/cli/quickstart.html)."
> — [komorebi README, `## Overview`](https://github.com/LGUG2Z/komorebi/blob/master/README.md)

The site behind it is substantial: [`mkdocs.yml`](https://github.com/LGUG2Z/komorebi/blob/master/mkdocs.yml) has an `About` group (`index.md`, `design.md`), `Getting started` (installation, example configurations, troubleshooting), five `Usage` pages, seventeen `Common workflows` pages, and a **CLI reference with a page per command** — 30+ entries under `cli/`. Note what is on the site and *not* in the README: `design.md`, the architecture document. komorebi's README never explains its internals.

What komorebi deliberately **keeps** in the README, despite having a docs site that could hold it:

- `# Contribution Guidelines` (~400 words of enforceable rules)
- `# Development` (IntelliJ macro-expansion settings — two settings, nothing else)
- `# Logs and Debugging`, `## Restoring Windows`, `## Panics and Deadlocks` (log path `%LOCALAPPDATA%/komorebi/komorebi.log`, `komorebic restore-windows`, `--features deadlock_detection`)
- `# Window Manager State and Integrations` and `# Window Manager Event Subscriptions` (~850 words: Named Pipes with a PowerShell and a Node.js example, Unix Domain Sockets, a Rust client example, the notification schema, TCP)

That is a coherent line: **the README holds what a person needs while they are looking at the repository** — how to contribute, how to hand you a usable bug report, and how to build against the machine-readable interface. **The docs site holds what a person needs while they are using the product** — install, configure, per-command reference.

GlazeWM draws the same line at a different place because it has no docs site: its config reference (~280 of its 393 lines) lives in the README, but its *architecture* does not. `CONTRIBUTING.md` carries `## Codebase overview`, `### Crates`, `### Commands & events`, `## Container tree` — the GlazeWM README never enumerates its crates. AeroSpace, PowerToys and komorebi all do the same thing.

**Applied to Mosaix**, which has no docs site and should not build one yet:

| Content | Where | Why |
| --- | --- | --- |
| Positioning sentence, key features, screenshot | README | The four are unanimous |
| Default keybindings table | README | GlazeWM's `## Default keybindings` cheatsheet is the single most-referenced part of its README; Mosaix's `Ctrl+Alt+*` defaults are currently only findable in `crates/mosaix-config/src/defaults.rs` |
| Minimal config example + config path | README | GlazeWM precedent; ~20 lines, not the full schema |
| Full config schema, every field | Linked (a new `docs/configuration.md`) | GlazeWM is the only one that inlines this and it costs 280 lines |
| CLI command reference | Linked or `mosaix --help` | komorebi gives every command its own page; a README table of the top 6 with a pointer is the proportionate version |
| Crate-by-crate layout of the 12 crates | **Linked** — `ARCHITECTURE.md` §16 already has it | GlazeWM keeps this in CONTRIBUTING; komorebi in `docs/design.md` |
| Domain vocabulary (zone, profile, resolved config…) | **Linked** — `CONTEXT.md` | No comparable inlines a glossary |
| Design rationale | **Linked** — `docs/architecture-decisions/` | komorebi's `design.md` is site-only |
| Build from source, test, toolchain | **Linked** — a new `CONTRIBUTING.md` | All four; currently Mosaix's only install path |
| Contribution rules (commit style, PR scope) | README *or* CONTRIBUTING | komorebi in README, other three link out; either is defensible |
| Log paths, how to file a good bug | README | komorebi's `# Logs and Debugging` precedent |
| IPC protocol / `mosaix state --json` | README (short) + link | komorebi's largest retained section is exactly this |

One rule of thumb that falls out of all four: **the README should link to a document at most one level deep, and never duplicate its content.** komorebi's README does not restate `design.md`; GlazeWM's does not restate the crate list. A Mosaix README that summarises `ARCHITECTURE.md` creates a second thing to keep true.

### Badges: what mature projects actually carry

Full inventory of the four, from raw source:

- **komorebi** (10, in a `<p>` block after the tagline, before the screenshot): Tech for Palestine, GitHub Actions workflow status for `windows.yaml`, GitHub all-releases downloads, commits-since-latest-release, a `badge/dynamic/json` count of active commercial licences, Discord, GitHub Sponsors, Ko-fi, Notado feed, YouTube subscribers.
- **GlazeWM** (3, after the H1 and tagline, defined as reference links at the bottom of the file): Discord invite, total downloads, and a hand-made `good_first_issues` badge linking to a filtered project board.
- **AeroSpace** (1, *inside* the `#` heading): `[![Build](.../workflows/build.yml/badge.svg?branch=main)]`.
- **PowerToys** (0).

Patterns that hold:

- **Every non-decorative badge is one of three kinds**: a live build/CI signal, an adoption signal (downloads, Discord members, sponsors), or a call to action (good first issues, sponsor, subscribe). komorebi's is heavily weighted to funding because it is a commercially licensed project.
- **Nobody badges their tech stack.** No "Made with Rust", no framework badge, no OS badge. Mosaix currently has four of these.
- **Nobody carries a static license badge.** The three permissive-licensed projects say nothing; komorebi, whose licence is genuinely unusual, spends three prose sections on it instead.
- **Nothing here uses `crates.io` / `docs.rs` badges** — correct for applications rather than published libraries, which is Mosaix's case too.
- **Placement is uniform**: immediately after the H1 and one-line description, before the hero image or immediately after it. AeroSpace's inline-in-heading variant works because there is only one.

For Mosaix today the honest badge count is **zero to one**. There is no CI workflow, no release, no Discord and no download count, so every badge available is decorative. The right move is to add a CI workflow first and then a single build-status badge — the AeroSpace shape.

### Install and build: two audiences, two documents

Unanimous across the four: **the README's install section is the end-user path only, and it is short.**

- **komorebi** — `# Installation` is four lines: a link to "A [detailed installation and quickstart guide]" and an embedded YouTube thumbnail. Package managers are named (`scoop`, `winget`) but not demonstrated. Zero commands in the README.
- **GlazeWM** — releases link first, in bold, then three package managers with one command each: `winget install GlazeWM`, `choco install glazewm`, `scoop bucket add extras && scoop install extras/glazewm`. No build step.
- **AeroSpace** — one command, `brew install --cask nikitabobko/tap/aerospace`, plus a `> [!NOTE]` about notarization and a bare URL for "Other installation options". Build-from-source appears only as `## Development`: "A notes on how to setup the project, build it, how to run the tests, etc. can be found here: [dev-docs/development.md]".
- **PowerToys** — four `<details>` blocks (GitHub `.exe`, Microsoft Store, WinGet, community tools), with the first one `open`, above a link to `learn.microsoft.com/windows/powertoys/install` for "detailed installation instructions and system requirements". Build guidance is one clause inside `## Contributing`: "please read the [developer docs](./doc/devdocs) for a detailed breakdown. This includes how to setup your computer to compile."

The build-side documents show what belongs there: AeroSpace's `dev-docs/development.md` has `## Install dependencies`, `## Create codesign certificate`, `## Entry point scripts`, `## IDE`, `## Xcode`, `## Tips`; GlazeWM's `CONTRIBUTING.md` has `### Setup` with `cargo build && cargo run` plus `### Tips` and the codebase overview.

**For Mosaix specifically**, which has no releases yet, the resolution is not "put the build steps in the README anyway" — it is to say so and route. A `## Installation` that reads "No binary releases yet. Build from source: see [CONTRIBUTING.md](CONTRIBUTING.md)" is three lines, matches komorebi's shape exactly, and gets fixed by adding a release rather than by rewriting. The multi-process detail — that `mosaix-agent.exe` is the daemon, `mosaix.exe` the CLI, and the Tauri settings app a separate `npm run tauri dev` target — is developer information and belongs with the build steps, not in an install section. What *should* stay in the README is the first-run fact: the agent writes `%APPDATA%\Mosaix\config\config.toml` on first run and adds a tray icon.

### What a README should deliberately not contain

Each of these is an omission visible in the comparables, not an abstract rule.

- **No architecture or crate breakdown.** All four exclude it: GlazeWM to `CONTRIBUTING.md`, komorebi to `docs/design.md`, AeroSpace and PowerToys to their dev docs. Mosaix's `ARCHITECTURE.md` §6 "Logical architecture" and §16 "Repository structure" already own this; the README should link once.
- **No glossary.** None of the four defines its domain terms in the README. `CONTEXT.md` is the right home.
- **No roadmap detail.** PowerToys' `## 🛣️ Roadmap` is **two sentences** pointing at a milestone. AeroSpace's `## Project status` is the exception that proves the rule — it is a checklist of eight *linked issues*, so the README holds pointers and the issue tracker holds the state. Mosaix's current one-paragraph "Not yet built:" list is close to right in spirit but should shrink and point at issues.
- **No changelog or release notes.** PowerToys' `## ✨ What's new?` is a banner image plus one line linking `/releases`. komorebi has no changelog section at all.
- **No manual table of contents.** Zero of four. GitHub auto-generates one from the headings.
- **No exhaustive feature enumeration.** GlazeWM: 5 bullets. AeroSpace: 8, each a link. PowerToys lists its 30+ utilities as a **table of links to `aka.ms` docs pages** — a name and an icon each, no descriptions. Mosaix's 13 multi-line bullets, several carrying edge-case behaviour ("surplus cells are left empty and surplus windows are reported rather than dropped"), are documentation wearing a feature list's clothes.
- **No test-suite instructions.** None of the four mentions running tests in the README; all put it in the contributing/dev doc. Mosaix's README currently ends its install section with `cargo test --workspace`.
- **No decorative badge wall.** Covered above.
- **No license text or long license discussion** unless the licence is genuinely non-obvious (komorebi's is; MIT is not).
- **No `## Support` / `## FAQ` before the product is explained.** GlazeWM's `## FAQ` is last, four questions, all operational.

One anti-pattern the comparables *do* commit, worth avoiding: komorebi opens with **three `## Note:` sections about mobile device management and a Mac fork before its `## Overview`** — 120 lines of licensing-enforcement caveats ahead of any statement of what the software is. It is a symptom of a commercial licence, not a model.

### Is there a convention worth adopting?

**standard-readme**: the [spec](https://github.com/RichardLitt/standard-readme/blob/main/spec.md) requires, in order, Title, Short Description (<120 characters), Table of Contents, Install, Usage, Contributing, License ("Must be last section"), with optional Banner, Badges, Long Description, Security, Background, API, Maintainers, Thanks. **None of the four comparables complies**: none has a TOC, PowerToys and AeroSpace have no License section, komorebi's License sections are in the middle, and none carries the standard-readme badge the spec suggests. The spec is aimed at libraries and packages; its `API` section and its TOC requirement both misfire for a desktop application on GitHub, which generates the outline itself. **Not worth adopting as a spec.** Two of its rules are worth stealing regardless: the sub-120-character description, and a fixed section order that does not drift between edits.

**GitHub community standards**: worth adopting, because it is a checklist of files rather than a README structure — README, CODE_OF_CONDUCT, LICENSE, CONTRIBUTING, plus issue templates in `.github/ISSUE_TEMPLATE` and a security policy ([GitHub Docs](https://docs.github.com/en/communities/setting-up-your-project-for-healthy-contributions/about-community-profiles-for-public-repositories)). All four comparables satisfy most of it; PowerToys satisfies all of it and adds `COMMUNITY.md`, `SUPPORT.md` and `DATA_AND_PRIVACY.md`. Mosaix satisfies two of four. The relevant consequence for the README is that adding `CONTRIBUTING.md` gives the README somewhere to send the from-source path.

**Rust API Guidelines, Documentation section**: mostly inapplicable. C-CRATE-DOC, C-EXAMPLE, C-QUESTION-MARK, C-FAILURE, C-LINK and C-HIDDEN are rustdoc rules for published library crates ([api-guidelines](https://rust-lang.github.io/api-guidelines/documentation.html)); Mosaix's crates are internal to one application workspace. Two items do apply: **C-METADATA** (`description`, `keywords`, `categories` are missing from `[workspace.package]`) and **C-RELNOTES** ("Release notes document all significant changes") — which is the same gap as having no releases to badge.

**GitHub's own guidance** is the shortest and the one all four converge on: a README should say "What the project does", "Why the project is useful", "How users can get started with the project", "Where users can get help with your project", and "Who maintains and contributes to the project" ([About READMEs](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-readmes)). Mosaix's current README answers the first and, partially, the third. It does not answer *why* (no comparison to FancyZones, PowerToys or komorebi, which is the question every Windows reader will have — komorebi devotes a whole `# Comparison With Fancy Zones` section to it), *where to get help* (no issues link, no discussions), or *who maintains it*.

## Sources

### Comparable project READMEs (raw source)

- [komorebi README](https://github.com/LGUG2Z/komorebi/blob/master/README.md) — 515 lines; H1 + tagline + 10 badges + screenshot; three `## Note:` licensing preambles; `## Overview` deferring all user docs to the site; `## Community`; three licensing/sponsorship sections; `# Installation` (4 lines, link + video); `# Comparison With Fancy Zones`; `# Demonstrations`; `# Contribution Guidelines` with 4 subsections; `# Development`; `# Logs and Debugging`; `# Window Manager State and Integrations`; `# Window Manager Event Subscriptions` with 6 subsections; `# Appreciations`
- [komorebi `mkdocs.yml`](https://github.com/LGUG2Z/komorebi/blob/master/mkdocs.yml) — the nav proving the split: `About` (index, **design.md**), `Getting started` (installation, example-configurations, troubleshooting), 5 `Usage` pages, 17 `Common workflows` pages, and a per-command `CLI reference`
- [komorebi `CONTRIBUTING.md`](https://github.com/LGUG2Z/komorebi/blob/master/CONTRIBUTING.md) — the contribution-licensing half that the README's guidelines section links to
- [GlazeWM README](https://github.com/glzr-io/glazewm/blob/main/README.md) — 393 lines; centred logo/H1/tagline/3 badges/nav row/demo video; `### 🌟 Key features` (5 bullets); `## Installation` (releases + winget/choco/scoop); `## Contributing` (3 sentences + link); `## Default keybindings` (cheatsheet image); `## Config documentation` with 7 `### Config: *` subsections; `## FAQ`; badge definitions as reference links at EOF
- [GlazeWM `CONTRIBUTING.md`](https://github.com/glzr-io/glazewm/blob/main/CONTRIBUTING.md) — `### Setup` (`cargo build && cargo run`), `## Codebase overview`, `### Crates`, `### Commands & events`, `## Container tree` — the architecture the README omits
- [AeroSpace README](https://github.com/nikitabobko/AeroSpace/blob/main/README.md) — 172 lines; `# AeroSpace Beta` with an inline CI badge, right-floated icon, one-line description, `Videos:` and `Docs:` link lists; `## Key features`; `## Installation` (one `brew` command); `## Community, discussions, issues`; `## Project status`; `## Development` (a single link); `## Project values` with an explicit **Non Values** list; `## macOS compatibility table`; `## Sponsorship`; `## People who have write access`; `## Tip of the day`; `## Related projects`
- [AeroSpace `dev-docs/development.md`](https://github.com/nikitabobko/AeroSpace/blob/main/dev-docs/development.md) — the build path: dependencies, codesign certificate, entry-point scripts, IDE, Xcode, tips
- [PowerToys README](https://github.com/microsoft/PowerToys/blob/main/README.md) — 121 lines, **zero badges**; `<picture>` hero + centred H1 + one-line description + centred nav row; `## 🔨 Utilities` (a link table, no descriptions); `## 📦 Installation` (4 `<details>` blocks + a link to learn.microsoft.com); `## ✨ What's new?`; `## 🛣️ Roadmap` (2 sentences); `## ❤️ PowerToys Community`; `## Contributing` (links `CONTRIBUTING.md` and `doc/devdocs` for compile setup); `## Code of conduct`; `## Privacy statement`

### Conventions

- [standard-readme spec](https://github.com/RichardLitt/standard-readme/blob/main/spec.md) — required sections in order (Title, Short Description <120 chars, Table of Contents, Install, Usage, Contributing, License-must-be-last) and optional ones; badge placement rules; the TOC exemption for READMEs under 100 lines
- [GitHub Docs — About community profiles for public repositories](https://docs.github.com/en/communities/setting-up-your-project-for-healthy-contributions/about-community-profiles-for-public-repositories) — the checked files: README, CODE_OF_CONDUCT, LICENSE, CONTRIBUTING, plus `.github/ISSUE_TEMPLATE` and a security policy
- [GitHub Docs — About READMEs](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-readmes) — the five questions a README answers; automatic table-of-contents generation from section headings; README discovery order (`.github`, root, `docs`)
- [Rust API Guidelines — Documentation](https://rust-lang.github.io/api-guidelines/documentation.html) — C-CRATE-DOC, C-EXAMPLE, C-QUESTION-MARK, C-FAILURE, C-LINK, **C-METADATA**, **C-RELNOTES**, C-HIDDEN; rustdoc-scoped, so only the last two apply to an application workspace

### Mosaix (for the gap analysis)

- [`README.md`](../../README.md) — 82 lines; the six static badges on line 3; the "Windows 11 and macOS" banner and tagline; 13 feature bullets; an install section that is a build section
- [`ARCHITECTURE.md`](../../ARCHITECTURE.md) — 1001 lines, 24 sections; §6 Logical architecture, §16 Repository structure, §17 Testing strategy, §21 Explicit non-goals — the content the README must link to rather than restate
- [`CONTEXT.md`](../../CONTEXT.md) — 207 lines; the `## Language` glossary (zone, zone cycle, base config, profile, resolved config, balanced grid, grid reflow, …)
- [`Cargo.toml`](../../Cargo.toml) — 13 workspace members; `[workspace.package]` carries only `version`, `edition`, `license`, `repository` (no `description`/`keywords`/`categories`)
- `crates/mosaix-platform-macos/src/lib.rs` — one line, contradicting the README's macOS claim
- [`docs/research/brand-mark-and-readme-hero.md`](brand-mark-and-readme-hero.md) — prior research on the hero image, including that GitHub's sanitizer strips inline `<svg>` so a hero must be a referenced image file
