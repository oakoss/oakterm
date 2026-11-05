---
title: Rex Architecture Review
date: 2026-11-04T23:31:07
scope: Superlogical's Rex (app build 1758, running server dd9a201) — architecture, agent surface, and gaps vs OakTerm
---

# Rex Architecture Review

## Scope

[Rex](https://www.superlogical.com/updates/public-testing-beginning) is a terminal and multiplexer from Superlogical, whose team includes Mitchell Hashimoto (Ghostty). Superlogical opened public testing in 2026, pitching Rex as a "drop-in replacement for your current terminal" with persistent sessions and program activity status, which overlaps OakTerm's Phase 1 and agent work. Rex is closed source, so this review works from the installed macOS beta and Superlogical's published docs:

- the app bundle;
- `go version -m` on the server binaries;
- the `rex` CLI;
- the server's self-describing API (`rex api list`, `rex api describe`);
- read-only block calls on a live pane.

Claims about behavior cite a command run on 2026-11-04 or 2026-11-05. Claims from documentation name their source, and claims drawn from binary symbols are labeled inferred.

## Rex as of 2026-11-05

### Process model

- **The server.** It is written in Go (`rex server run`, Go 1.27.3) and runs detached from the app (parent pid 1), listening on a Unix socket under `~/Library/Application Support/rex/`. It owns the PTYs, so sessions survive app restarts.
- **Launch flags.** They cap resources (`--max-sessions 64 --max-blocks 1024 --max-windows 64 --max-panes 64`) and allow a websocket origin of `https://rex.superlogical.com`, which points to a web client.
- **The app.** The macOS app (`Rex Beta.app`, SwiftUI/AppKit, updated through Sparkle) is a client.
- **Updates leave the running server alone.** App build 1758 bundles server `d018d2d` (tailscale.com v1.104.4) in `Contents/Helpers` and staged it at `RexServer/Versions/1758/`. The server actually running is the older `Versions/Current` binary (`dd9a201`, tailscale.com v1.104.2), started before the update. So an app update does not restart the server, and the app keeps talking to it across the version gap. `go version -m` on both copies printed the two revisions. Every measurement below comes from that running `dd9a201` server.

### Terminal core

Rex leaves VT emulation to Ghostty's library instead of writing its own:

- The server links `go.mitchellh.com/libghostty`, a cgo binding to `libghostty-vt`. The [Ghostty README](https://github.com/ghostty-org/ghostty) describes that library as MIT-licensed, stable in function, and still changing its API.
- The client links Swift modules named `GhosttyTerminal`, `GhosttyVT`, and `GhosttyRender`. `GhosttyRender` is a Metal renderer with a glyph atlas, CoreText shaping, Kitty image processing, minimum-contrast correction, and cursor effects.

### Wire model (inferred)

- **Client replicas.** The client binary contains `GhosttyTerminal.Terminal.SnapshotDecoder` and `GhosttyVT.SnapshotDecoder`, which suggests each client keeps a terminal replica, seeded by a snapshot.
- **Raw bytes after the snapshot.** A [third-party write-up](https://www.bitdoze.com/superlogical-rex/) says the server then forwards raw PTY bytes for clients to parse. That source is wrong elsewhere (see Corrections), so this is unconfirmed.
- **Separate data connection.** Terminal data travels on a separate connection, opened with a single-use ticket (`data.connect`, subprotocol `rex.data.terminal.v1`).
- **Control encoding.** The control API runs over JSON and CBOR (`fxamacker/cbor` is in the server's dependencies).
- **Not observed directly.** `rex block stream` printed `command "rex block stream" is not implemented`.

### Object model

From `rex api list`:

- A session holds windows (tabs). A window holds blocks in a split layout, plus floating layers (`session.new_layer`, `raise_layer`, `resize_layer`).
- Blocks can be detached and re-placed without closing them (`detach_block`, `move_block`).
- Blocks come from pluggable *block creators* (`block_creator.list`). The beta ships two: `com.superlogical.terminal`, and `com.superlogical.onboarding`, which is the setup walkthrough built as a block.
- Each creator exposes its own methods. The terminal's include `format`, `modes`, `process`, `pwd`, `title`, `list_dir`, `program_status`, `write`, `resize`, and `set_theme`.

### Multi-client sizing

A terminal has an *owner* client (`session.own_block`, `disown_block`). `com.superlogical.terminal.resize` and `set_theme` take effect at once when the caller owns the block or no client does. Otherwise they wait until the caller becomes owner. `size` reports the size each client last requested.

### Shell launch and process introspection

`rex block call process` on this review's pane printed:

- the PTY's child: `/usr/bin/login`, running as root;
- the shell under it: `-zsh`;
- the foreground process, with its path, `argv0`, and cwd;
- the full chain of ancestor processes.

`ps` on that `login` process printed `/usr/bin/login -q -flp <user> /bin/bash --noprofile --norc -c … exec -a "-$name" -- "$@" rex-login zsh /bin/zsh`:

- `-p` keeps the server's environment rather than replacing it (`man login`).
- The bash trampoline execs zsh with `argv0` set to `-zsh`, which makes it a login shell that reads the profile files.

How that environment compares between a GUI-launched and a terminal-launched server was not measured.

Agents can read which process holds a pane without any shell integration.

`rex block call modes` lists the modes programs can turn on, among them `synchronized_output`, `grapheme_cluster`, `report_color_scheme`, `in_band_size_reports`, and `kitty_paste_events`.

### Agent surface

- **CLI.** The `rex` CLI (built with Cobra) wraps a schema-described API: `rex api describe <method>` prints JSON Schemas for input and output, with examples.
- **Agent guide.** `rex skill show` prints a guide bundled for agents.
- **Session hook.** `~/.claude/settings.json` registers a Claude Code SessionStart hook (`rex __internal session-start --agent claude`), and every Rex pane's shell carries `REX_SESSION` and `REX_BLOCK`. `rex __internal session-start --help` describes the command as "Print the output of the SessionStart hook". Running it printed a context block: the pane's server, session, and block IDs, how to use the `rex` CLI and skill, and, under a `--feature-session-naming` flag that defaults to on, an instruction to name the session after its task while handling the first request.
- **Program status (OSC 7501).** Programs report their state in-band with an escape sequence, documented in [program status](https://www.superlogical.com/rex/docs/build/program-status.md): `ESC ] 7501 ; state=…:kind=…:progress=…:app=…:title=<b64>:msg=<b64> ST`.
  - The documented states are `idle`, `working`, `done`, `blocked`, `error`, and `clear`.
  - `kind` is `permission`, `question`, or `auth`, and applies only to `blocked`.
  - `rex api describe com.superlogical.terminal.program_status` says to "Treat a value you don't know as 'idle'", so the state set is open, not closed.
  - `ESC ] 7501 ; ? ST` asks whether the terminal supports it. The client binary contains the reply `7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth`.
  - Per the docs, `working` and `blocked` clear when the process exits or the shell prompts again, and `done` and `error` persist.
- **Claude Code emits it.** `rex block call program_status` on this review's pane returned:
  - a record from Claude Code (`"state":"working","app":"claude-code"`), whose `msg`, when set, described the tool call in progress;
  - a second record for a running subagent, with its own `id` and a `title` naming the subagent's task.

  Those records change with every tool call, but `~/.claude/settings.json` registers only a SessionStart hook for Rex, so the hook cannot be producing them. The [Claude Code changelog](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md) records the feature: "Added Program Status Protocol (OSC 7501) support: terminals that implement it can show whether Claude Code is working, waiting on you, or done." Claude Code appears to send reports only after the terminal answers the `OSC 7501 ; ?` support query (inferred from the Claude Code binary; the bytes themselves were not captured).
- **MCP.** `rex api list` lists no MCP methods, and `strings` finds no `modelcontextprotocol` in either binary, so the local server exposes no MCP control surface. Superlogical also hosts a separate MCP endpoint (`https://www.superlogical.com/rex/community-mcp`). Per the [docs index](https://www.superlogical.com/rex/docs/llms.txt), it serves the docs, the changelog, and Rex Beta Discord discussions, read-only, to Discord members signed in through OAuth. It is not a control surface.
- **Events.** Per the [events reference](https://www.superlogical.com/rex/docs/reference/events.md), Rex emits session, terminal, and server events, such as `terminal.pwd_changed`, `terminal.child_exited`, `terminal.bell`, and `session_created`. `terminal.process_changed` fires when the foreground process changes. Lua handles events with `rex.on` and `rex.wait`, and `rex events --json` streams them from the CLI, so an agent can watch rather than poll.

### Remote

- **Embedded Tailscale.** The server embeds Tailscale (tsnet). `rex api call server.status` reported `"tailscale":{"state":"running", …, "allow_tailnet_members":false, "https":{"state":"available"}}`. By default, only the machine's own Tailscale user is admitted.
- **Hosts.** Remote servers are addressed as *hosts* (`hosts.list`, `hosts.put`).
- **Identity.** [Superlogical's remote-access docs](https://www.superlogical.com/rex/docs/linux/remote-access.md) say identity comes only from the transport today, and that SSH-key auth is planned.
- **Server modes.** Per the [server modes docs](https://www.superlogical.com/rex/docs/linux/server-modes.md), Linux and FreeBSD offer a personal server (your own account, started by any `rex` command), a shared server (one `rex` account), and a root login server. Per the [login server docs](https://www.superlogical.com/rex/docs/linux/login-server.md), the login server identifies each caller by transport (a Tailscale login or a local uid), looks up the accounts that caller may use in `/etc/rex/accounts.jsonc`, and opens a PAM login session for that account. It has no passwords or keys yet.

### Terminfo

Rex ships its own `xterm-rex` entry, plus an ssh guard. `rex terminfo status` printed `downgrade: in effect, from Rex's guard`: an ssh config include resets `TERM` for hosts that lack the entry, and `rex terminfo host` installs the entry on remote machines.

### macOS privacy proxying

- **Usage strings.** The app's `Info.plist` declares them for the camera, microphone, audio capture, contacts, calendars, photos, location, Bluetooth, and AppleEvents, each worded "A program running in a Rex terminal wants…".
- **File access.** The server exposes `server.apple.grant_file_access` ("Let the server, and every program it runs, read files a Mac app was given") and `server.apple.privacy_status`.

### Other

- A `RexMigrate` resource bundle and the onboarding walkthrough ("migrating an existing setup") import settings from other terminals.
- `init.lua` holds key bindings and actions (`rex.bind`, `rex.action`). Clients can also load their own Lua scripts (`client.set_script`), and `rex do` runs a Lua script against a server.
- Per the [Lua API reference](https://www.superlogical.com/rex/docs/reference/lua.md), Rex runs Lua 5.1 (the server links `gopher-lua`) with the full standard library, including `io`, `os`, and `require`. Only `io.popen` and `os.execute` raise errors, and `rex.exec` replaces them.
- [Key modes](https://www.superlogical.com/rex/docs/customize/key-modes.md) are named binding sets ("also called 'key tables' in other software"), such as a resize mode. By default, a key the mode doesn't bind keeps its usual action; `exclusive = true` limits keys to the mode's own bindings. Key sequences bind keys pressed one after another.
- Per the [privacy page](https://www.superlogical.com/rex/docs/reference/privacy.md), Rex collects no telemetry. The page lists every outbound connection Rex makes, such as the macOS app's daily update check.
- The docs have separate action and settings references for the macOS and iOS apps, and tag features by update channel (Snapshot, Bleeding Edge, Preview).
- When a program writes to the clipboard (OSC 52), the server holds the text for about a minute (`clipboard_read`).
- Per the [installation docs](https://www.superlogical.com/rex/docs/installation), the macOS app plus Linux and FreeBSD servers are in public testing, and iOS, Linux, and Windows clients exist.
- Rex is free and closed source, and Superlogical has promised "some open-source releases".

### GUI and settings

Observed on 2026-11-04 and 2026-11-05: a demo session built with the `rex` CLI (four panes reporting each OSC 7501 state, a floating pane, a second tab), captured by window ID, plus screenshots of each top-level Settings page. The screenshots stay out of this public repository because they show account and network details.

**Layout.**

- **Panes are cards.** In the "Comfortable" pane style, each pane is a rounded card with a gap around it.
- **Pane headers.** Each card has an optional header: a per-process icon (`zz` for `sleep`, `>_` for a shell), the process name, then the cwd. Split, float, zoom, and close controls show on the focused pane and on hover.
- **Focus.** The focused pane gets a faint outline. A floating pane is the same card, raised with a shadow.
- **One merged bar, no titlebar.** With tabs "Above", the traffic lights, a session switcher, the tabs, and the activity, palette, and new-tab buttons share one row.
  - The tabs sit in one shared track, with the active tab as a raised pill.
  - A tab with splits shows its panes' icons stacked.
- **Tab position.** "Left Side" and "Right Side" move the tabs into a sidebar that groups them under session headers.
- **Compact panes.** A capture with panes edge to edge, a thin divider, and headers kept matches the "Compact" style (inferred; the setting itself was not in that capture).
- **Pane Behavior.** A sub-page of Interface sets:
  - padding (X and Y), and a padding "Color" of Background, Extend, or Extend Always (set to Extend), which decides whether edge-cell backgrounds carry into the padding;
  - an active-pane edge highlight, with a color, opacity, and animation ("Fade");
  - fading of inactive panes ("Surface & Content") and of content in inactive windows;
  - autobalanced splits, and focus follows mouse.

**Overlays.**

- **Command palette** (⇧⌘P).
  - **Empty.** It lists commands ("Search commands, sessions, and tabs…") with right-aligned shortcuts and chevrons for commands that open a submenu.
  - **With a query.** It groups results into "Tabs" (each with its session name, app icon, and a check on the current tab) and "Commands".
  - **Footer.** Hints for "Change Keybinding ⌘↩" and "Action ↩". The keybinding hint is greyed out when a tab row is selected. Rebinding itself was not tried.
- **In-app notification.** "Check for Updates" showed a notice at the top right of the pane: a dark, rounded pill with a check icon, the message "Rex Beta is up to date", and a close button. Whether program-status changes raise the same notice was not reproduced: a `blocked` report from a visible pane, and one from a hidden tab, showed none in captures taken about once a second.
- **Find** (⌘F). A floating, rounded bar at the top right of the pane, with a match count ("1/1"), previous and next arrows, and close. The match is highlighted in the text.
- **Selection.** A drag selection over plain shell output in a zsh pane (selection colors were set to "Theme" in Settings).
  - **Fill.** A solid pale-rose fill, with the text in a dark color.
  - **Extent.** Interior lines run from the pane's left edge, through the horizontal padding, to its right edge. The last line ends just past its final character.
  - **Corners.** The end of the last line is rounded, on both its outer and inside corners, at about 2pt ("Rounded corners" is on). Corners against the pane edge are square.
  - **Copy.** Releasing the drag copied the selection to the clipboard (`pbpaste` returned it) and played a brief shimmer, matching the "Copy on selection" and "Play animation on copy" settings.

**Activity.** Program status does not appear in pane headers. Tabs of agent sessions showed working dots ("Show activity in tabs"), but the demo's tab, holding all four reporting panes, showed no indicator.

- **Where it shows.** An activity popover lists every reporting pane, grouped by session: an app icon, a name, the status `msg`, and an age.
- **How each state renders.**

  | State | Shown as |
  | --- | --- |
  | working | dots |
  | blocked | a grey "Blocked" pill |
  | error | a red "Failed" pill, plus a red dot on the row's icon |
  | done | no badge |

- **Name, not title.** The rows showed the process name, not the `title` field the demo programs sent.
- **Settings.** Notifications route each state separately to tab activity, system notifications, the Dock badge, and sound. An experimental option groups activity as Blocked, Failed, Working, Done.

**Appearance.**

- **Interface style.** "Classic" or "Glass", with no description on the page; presumably the macOS translucent material (inferred). A note reads "Pane background colors ignore the theme when Glass is active." A separate "Use glass for panes" toggle exists, and its exact effect was not checked.
  - **Classic in light mode** (Catppuccin Latte). A 6×6 grid of full-resolution samples across the content, plus samples along the title row, is a uniform (239,241,245), exactly Latte's base `#eff1f5`.
  - **Glass in light mode.** The same grid measured lighter and near-neutral, from (242,247,249) to (251,248,248). Cells near the blue desktop were bluer, and the title area is a vertical gradient that blends into the content rather than a separate band. That suggests a translucent material over the desktop, rather than a flat theme color (inferred).
- **Themes.** Light and dark themes are picked separately, from groups labelled "Custom" with a "New" button (visible: Catppuccin, Kanagawa, Rose Pine, and TokyoNight variants). Ghostty and iTerm theme files import, and each is assigned to light or dark by its background color.
- **Animation.** "Animation style" is Snappy, Natural, or Off.

**Terminal settings.**

- **Contrast and color.** "Improve text contrast" and "Force theme palette for text". "Use Display P3 color space" ("Read theme and program colors as wide-gamut Display P3") was off on this install.
- **Smooth terminal resizing.** Described as "Reflow text on the frame a pane changes size, without waiting for the server."
- **Selection.** Copy on select, an animation on copy, and rounded selection corners (see Selection under Overlays).
- **No keyboard copy mode.** The app's action list (`rex -C <client> actions`) has Find and jump to previous and next prompt, but no copy mode or keyboard selection action. Its only copy actions copy the pane ID, version info, debug info, and the config path. The [Lua API reference](https://www.superlogical.com/rex/docs/reference/lua.md) has no selection, clipboard, or copy function, so scripts cannot add one either: they can call the server API, block methods, and app actions, and none of these offers text selection.
- **Links.** Links that open apps (such as `ssh://`) were set to "Ask First". Web, mail, and file links open directly.
- **Titles.** "Prefer generated titles" shows the program and directory instead of titles the shell sets.
- **Paths.** A path browser (⌃⌘I) inserts paths, and recent paths are shared across sessions on the same host.
- **Shortcuts.** Shortcuts recorded in Settings override `init.lua`.

**Server and integrations.**

- **Server update prompt.** The Server page showed "New API features are available. Update the server to enable them." This is the version gap from [Process model](#process-model), surfaced to the user.
- **Remote Control.** "Remote Control" ("Let Rex and other clients perform actions in this app") was off on this install. With it off, `rex -C <client> do client.sidebar.toggle` failed with `Rex.app is not accepting actions. Turn on Remote Control in its Settings under Rex Server.`
- **Agent integrations.** Integrations install a skill and session hooks for Claude Code, Codex, OpenCode, and Pi. "Let agents name sessions" lets an agent relabel its session; this review's session was relabeled during the work.
- **Block plugins.** `rex server run --help` lists `--block-dir DIR`, "discover executable block plugins in DIR", so third parties can add block types as external programs.

## Findings

### Corrections

- **Pane tree.** The bitdoze write-up says Rex has "no tmux-style pane tree". The API contradicts it: `session.new_split`, `resize_pane`, `swap_blocks`, `focus_direction`, and `zoom_block` describe a split layout within each window. Treat that source as unreliable on architecture.

### Validated Decisions

- **The daemon owns the terminal; the GUI is a viewport** ([ADR-0007](../adrs/0007-daemon-architecture.md)).
  - Rex makes the same split and keeps sessions alive the same way.
  - Its update model shows the version gap [ADR-0020](../adrs/0020-daemon-upgrade-version-skew.md) plans for: after an update, the new app talks to an older server that is still running.
- **The CLI is the agent-control surface** ([ADR-0021](../adrs/0021-agent-control-api.md)).
  - Agents drive Rex through the `rex` CLI plus a bundled skill, the shape OT-288 plans for OakTerm.
  - The local server has no MCP surface.
- **Sandboxed Lua config** ([ADR-0005](../adrs/0005-lua-sandboxed-config.md)). Rex also chose Lua (gopher-lua), but without the sandbox: its config can read and write files through `io` and run commands through `rex.exec`. ADR-0005 keeps `io` and `os` out of config and leaves capabilities to WASM plugins. Rex shows the convenience that trade gives up, and nothing here argues for reversing it.
- **Key tables for modal contexts** ([ADR-0011](../adrs/0011-keybind-dispatch.md)). Rex's key modes are the same idea, down to a resize mode (OT-136). ADR-0011's modal tables (copy and resize) drop unbound keys, while Rex modes let them through unless `exclusive = true`.
- **Keyboard copy mode** ([ADR-0012](../adrs/0012-copy-mode-scrollback-access.md), OT-100). Rex lists no copy mode or keyboard selection action, so OakTerm's copy mode has no counterpart there.
- **Agent state vocabulary** ([ADR-0023](../adrs/0023-agent-state-vocabulary.md)), with caveats. OSC 7501's states map onto ADR-0023's, but the shapes differ:
  - **Lifecycle and outcome.** Rex makes `error` a peer of `done`, combining lifecycle and outcome, the shape ADR-0023 rejected as Option A.
  - **Missing and different states.** Rex has no `unknown`. Its `idle` means "not doing anything" rather than ADR-0023's "done and acknowledged".
  - **Open versus closed.** Rex treats unknown states as `idle`, where ADR-0023's set is closed.
  - **`kind` versus detail.** Rex's `kind` is a fixed three-value set, unlike ADR-0023's free-form detail string.
  - **Verdict.** Importing OSC 7501 means a mapping (`error` → `done`+`error`, `kind` → detail), not adopting Rex's model. ADR-0023 stands.
- **Writing our own VT core** ([Spec-0002](../specs/0002-vt-parser.md)), with caveats.
  - **The alternative.** `libghostty-vt` is an MIT-licensed terminal state machine with a C API, which Rex, pyghostty, and others now build on.
  - **The cost.** Adopting it would add a Zig toolchain to the build, bring an unstable API, and replace work Phase 0 already finished.
  - **Verdict.** Don't switch. Recheck it if VT correctness bugs keep recurring (see OT-296, OT-303).
- **Reading colors as sRGB on a P3 surface.** OakTerm reads theme and program colors as sRGB and converts them to P3 for output on macOS (`SRGB_TO_P3` in `crates/oakterm-renderer/src/shaders.rs`, added in 642ef48), so colors look as their authors intended.
  - **The same default elsewhere.** Rex's "Use Display P3 color space" switch was off on this install, and Ghostty's [`window-colorspace`](https://ghostty.org/docs/config/reference) documents `srgb` as "the default".
  - **The opt-in mode.** Both offer reading color values as P3 instead, which would saturate them (inferred from the descriptions; not tried).
  - **Deferred.** OakTerm defers that mode until someone asks for it. This review is the record of that choice; no ADR covers it.
- **One renderer across platforms** ([ADR-0018](../adrs/0018-gpu-rendering-wgpu.md)) contrasts with Rex.
  - Rex's macOS client uses its own Metal renderer, not a cross-platform layer.
  - The iOS, Linux, and Windows clients were not examined. If each has its own renderer, Rex maintains several where OakTerm, using wgpu, maintains one.

### Challenged Decisions

- **Dirty-row pull versus replica clients** ([Spec-0001](../specs/0001-daemon-wire-protocol.md)).
  - **OakTerm's model.** The daemon parses, and clients pull `RenderUpdate` diffs, which keeps clients thin for a Phase 4 phone or web client.
  - **Rex's likely model.** The client symbols suggest full terminal replicas, and the "Smooth terminal resizing" setting says clients reflow text locally "without waiting for the server", which suggests one (inferred). That would let selection, search, and accessibility run locally too, at the cost of a VT engine in every client. Whether Rex runs those locally was not measured.
  - **Recommendation.** Phase 4 remote access should choose between the two deliberately rather than inherit Spec-0001. This is an ADR candidate once Phase 4 is planned, not now.

### Missing Specs

- **Multi-client pane sizing.**
  - **Already open.** The [decisions audit](2026-07-01-214630-decisions-architecture-audit.md) listed multi-client focus and resize arbitration on shared panes as open. ADR-0007 still says only that multiple clients can view one pane.
  - **Rex's answer.** An owner client, plus the size each client requested, is one candidate design.
  - **Where it goes.** The decision belongs in Spec-0001 before multi-client attach ships.
- **Program-reported status over OSC.**
  - **The gap.** [ADR-0024](../adrs/0024-agent-state-sources.md) ranks an agent's own reports above heuristics, but lists only ACP, hooks, and `ctl self set-status`.
  - **Why OSC 7501.** It reports in-band, needs no socket access, and works over ssh. Claude Code emits it, including per-subagent records, and appears to wait for the terminal to answer `OSC 7501 ; ?` first (inferred), so OakTerm should answer that query.
  - **Where it goes.** The agent-session spec (OT-300) defines where it ranks. OT-317 tracks the work.
- **macOS privacy attribution.**
  - **The gap.** When a program the daemon spawned triggers a privacy (TCC) prompt, macOS attributes it to whichever process it deems responsible. No OakTerm doc covers usage strings, that attribution, or a way to grant file access.
  - **What's needed.** At least an idea-doc section before packaging (OT-267).

### Worth Borrowing

- **A self-describing control API.**
  - **What it is.** `api list` and `api describe` print JSON Schemas, so agents can discover `oakterm ctl` without a skill.
  - **Where it goes.** A candidate for the Agent Control API spec (OT-289).
- **Starting shells as login shells.** Rex runs `login -flp <user>` with a trampoline that execs `-zsh`, keeping the server's environment and letting the shell's profile files fill it in. That is a candidate for OT-179's "spawn login shells" option. Whether it fixes PATH for a GUI-launched daemon is untested.
- **Introspecting the process in a pane.** Rex reports the foreground process and its ancestors per pane. That is the data the process-detection tier of ADR-0024 needs, exposed to agents directly.
- **An event stream for agents.** `rex events --json` and `rex.on` in `rex do` scripts let a script react to cwd changes, foreground-process changes, and the pane's own program exiting, instead of polling. `terminal.child_exited` covers the program the pane started, usually the shell, not each command. A candidate for the Agent Control API spec (OT-289).
- **A page listing every outbound connection.** Rex's privacy page names each connection and when it happens. OakTerm's update check ([ADR-0003](../adrs/0003-update-check-policy.md)) could ship with a page like it.
- **Per-account login sessions for remote use.** The login server maps a transport identity to an OS account and opens a PAM session for it. That is prior art if multi-user daemon sharing is ever taken up. ADR-0023 revisits its daemon-global acknowledgment only if such sharing ever exists, and Phase 4's planned token and mTLS auth ([idea 29](../ideas/29-remote-access.md)) already covers the gap Rex admits: no passwords or keys yet.
- **Config migration** from other terminals as part of first run.
- **Floating layers and detachable blocks** as layout primitives, relevant to OT-102's floating panes.
- **Theme import.** Ghostty and iTerm theme files, assigned to light or dark by background color, plus separate light and dark picks. Relevant to OT-223.
- **Per-state notification routing.** Each agent state chooses its own surfaces (tab, system notification, Dock badge, sound), which fits ADR-0023's attention tiers.
- **Ask before links that open apps.** `ssh://` and similar links ask first, while web links open directly.
- **Layout choices as settings.** Comfortable or compact panes (the option names as seen in Settings), optional pane headers, tab position, and active-pane highlight and inactive-pane fading.
- **Rebinding from the palette.** The palette shows a "Change Keybinding" hint for the selected command (not exercised). A candidate for Spec-0009's palette (OT-101).
- **Find as a floating bar** with a match count, a reference for OT-126's search overlay.
- **Generated titles.** Showing the program and directory instead of titles the shell sets.
- **Executable block plugins.** `--block-dir` is a process-level plugin boundary, a contrast with ADR-0019's WASM components worth noting when Spec work on plugins starts.

### Not Borrowed

- **The terminfo ssh guard.** OakTerm already reports `TERM=xterm-256color` and ships no terminfo entry of its own ([idea 36](../ideas/36-terminal-fundamentals.md), [idea 10](../ideas/10-pain-points.md)), so remote hosts need nothing installed. Rex's guard solves a problem that design avoids. It matters only if OakTerm ever ships its own entry.

## Action Items

- **Tracking:**
  - OT-317 covers OSC 7501 support, including answering the `OSC 7501 ; ?` query, and is blocked by the agent-session spec (OT-300).
  - OT-179 has a comment citing Rex's `login -flp` + `-zsh` approach as an untested candidate.
  - OT-101, OT-126, OT-223, and OT-289 have comments pointing to the relevant sections of this review. OT-102 and OT-136 are cited here without a comment.
- **Spec-0001:** decide multi-client size arbitration before multi-client attach is scheduled.
- **Agent Control API spec (OT-289):** consider schema-described methods (`ctl api describe`), an event stream, and per-pane process introspection.
- **Phase 4 (later):** write an ADR on the remote wire model (replica versus diff).
- **Spec-0002:** recheck libghostty-vt if VT correctness bugs keep recurring (OT-296, OT-303).
- **No action:** the remaining Worth Borrowing items are prior art to consult when their area is designed.
- **Packaging (OT-267):** cover macOS privacy usage strings and attribution.
