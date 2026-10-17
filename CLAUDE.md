# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

KOBridge is a macOS and Linux app (Tauri 2 + Svelte 5 + Vite, Rust backend) that manages KOReader on a Kindle over SSH. The UI is English by default with a Spanish option (voseo: "Conectá", "Reiniciá"), chosen in the Settings view and saved as `Settings.lang`.

## Commands

```sh
bun install
bunx tauri dev                          # app with HMR; Rust changes rebuild automatically
bunx tauri build                        # .app + .dmg in src-tauri/target/release/bundle/
bunx tauri build --bundles app          # .app only (faster)
bun run check                           # svelte-check + tsc; must stay at 0 errors / 0 warnings

cd src-tauri
cargo test                              # unit tests (Lua parser, Markdown, path allowlist, quoting)
cargo test parses_sidecar               # single test by name
cargo test -- --ignored --nocapture     # live tests: the real Kindle, claude -p and the highlights cache
cargo test kindle_size -- --ignored --nocapture
```

The `--ignored` tests in `src-tauri/src/lib.rs` (`mod live`) need the Kindle online. `kindle_writes` creates and then deletes files: `/tmp/kob test/` and `koreader/patches/9-kob-test*.lua.disabled`.

Tauri CLI comes from `@tauri-apps/cli` via `bunx`. It is not installed with `cargo install`.

## Architecture

**SSH is the system binary.** `src-tauri/src/ssh.rs` spawns `/usr/bin/ssh` with `BatchMode`, `ConnectTimeout=4` and `ControlMaster`/`ControlPersist=600` (socket `~/.ssh/kob-%C`). Everything about reaching the device comes from the user's `~/.ssh/config`, so there is no IP setting in the app. The user's config uses `Match originalhost kindle exec "nc -z ..."` blocks to pick the IP per Wi-Fi, and `HostKeyAlias kindle` to keep one known_hosts entry.

Calling `ssh kindle` runs every `Match exec` probe each time, which measured ~2.1 s. To avoid that, `ssh::resolve()` parses `ssh -G kindle` once, and later calls target that IP with explicit `-p/-l/-i/-o HostKeyAlias` args, which costs ~45 ms on a warm mux.
- The resolved args live in `ssh::TARGET` and are persisted as `Settings.last_target`, so a relaunch connects in ~0.5 s.
- A connect-class error clears TARGET. `status` (`probe()`) then re-resolves once and retries.
- A `proxycommand`/`proxyjump` host keeps the plain alias.

Do not replace this with a Rust SSH crate.

Four transport primitives in `ssh.rs`; every feature is a thin wrapper:
- `run`: stdout as a String.
- `run_with_input`: writes remote files through `cat >` on stdin, so file content is never shell-quoted.
- `pull_to_file`: streams stdout to `<dest>.part`, then renames it; used by backup and stats.
- `upload`: streams a local file into `cat > name.kob-part && mv`. It avoids `scp`, whose remote side word-splits book names with spaces and parentheses.

`stream` spawns a long-running command such as `tail -f` for the log view.

**Remote commands are shell strings.** Interpolate any value only through `ssh::sq()` (single-quote escaping). `lib.rs::allowed()` restricts read, write, rename and delete to:
- `koreader/patches/<name>`
- `/mnt/us/fonts/<name>`
- `koreader/defaults.custom.lua`
- `koreader/settings.reader.lua`

Extend that allowlist rather than bypassing it. The Files view is the one exception: `files.rs` has its own guard for all of `/mnt/us` (see below).

**Commands (`lib.rs`)** map one-to-one onto the sidebar views in `src/lib/views/`. They are async Tauri commands that return `Result<T, String>`; the String is shown to the user, so build it with `ssh::tr(en, es)`, which follows `Settings.lang` (set in `get_settings`/`set_settings`). Streaming uses `tauri::ipc::Channel`: backup bytes and log lines. App settings are a JSON file in `app_config_dir()` (struct `Settings`, `#[serde(default)]` so new fields are backward compatible). Caches live in `app_cache_dir()`: the `statistics.sqlite3` snapshot and `highlights.json`. Stats and highlights therefore render offline.

**Device facts the code depends on:**
- **Busybox userland.** `tar` supports `-T` and the device has `sqlite3`, but no `rsync`.
- **Backup paths.** `backup_list_cmd()` writes `/tmp/kob-list`; both `backup_run` and `backup_size` (`du -sk` plus an 8 MB `dd` speed probe) use it.
- **Stats snapshot.** Stats are copied with `sqlite3 ... '.backup /tmp/kob-stats.db'`, never by copying the live DB. `stats.rs` queries `page_stat_data`, not the `page_stat` view.
- **Sidecar location.** Sidecars sit next to books (`*.sdr/metadata.*.lua`, `document_metadata_folder = "doc"`).
- **Highlights fetch.** One ssh call cats every sidecar that has annotations, each prefixed by an `@@KOB@@ <path>` line (`highlights::FETCH_CMD`).
- **Lua parsing.** `lua.rs` is a hand-written parser for KOReader's `%q` serialization subset. Only the modern `annotations` table is read; the device has 0 legacy `highlight` tables.
- **`settings.reader.lua` writes.** KOReader overwrites this file on exit, so `write_remote` refuses it while `reader.lua` is running. The check is server side, not just in the UI.
- **No remote restart.** KOReader cannot be restarted from outside: `koreader.sh` only relaunches on an internal exit code 85. The UI tells the user to restart it instead.
- **SSH server lifetime.** The dropbear SSH server survives a KOReader exit.
- **Exec length.** dropbear rejects an exec request over 9,000 bytes (`MAX_CMD_LEN`), so any command that grows with a selection goes through `ssh::run_script` (script on stdin to `sh`).
- **Storage layers.** `/mnt/us` is the `fsp` FUSE layer over the ext4 image `/dev/loop/0` mounted at `/mnt/base-us`. It is case-sensitive and has no symlinks. A file written to `/mnt/base-us` (MTP writes there) appears in `/mnt/us` at once; a delete can still list for ~1 s.
- **BusyBox quirks.** `sleep` takes integers only (`sleep 0.5` fails); use `usleep`. `find` has no `-printf`; `mv` has `-T` and `-n`.
- **Wi-Fi sleeps.** KOReader turns Wi-Fi off when the Kindle sleeps, so `ssh kindle` fails with "Could not resolve hostname" (every `Match` probe fails). Live tests need the Kindle awake; a wait-for-ssh loop beats retrying by hand.
- **macOS and MTP.** macOS's `ptpcamerad` claims an MTP device as soon as it appears, and libmtp clients then fail with `libusb_claim_interface() = -3` until it is killed or the app grabs the device first.

**Obsidian export (`obsidian.rs`).**
- Each highlight ends with an Obsidian block id `^kob-xxxxxxxx`, an FNV-1a hash of `datetime` and `text`. Never change the hash: re-export uses these ids to append only new highlights, so a changed id duplicates every quote. `block_id_is_stable` pins one value.
- `merge()` never rewrites user text. It appends new quotes at the end and only sets the app-owned frontmatter keys (`progress`, `highlights`, `last_read`, `read_time`, `enriched`). A note without any `^kob-` id comes from 0.1.1 and is renamed to `<title> (anterior).md`. `Libros.md` is app-owned and rewritten.
- `last_read`/`read_time` come from the cached stats snapshot, matched by the sidecar's `partial_md5_checksum` (same value as `book.md5`), falling back to title.
- Vaults are read from `~/Library/Application Support/obsidian/obsidian.json`.

**Claude enrichment.** `enrich_start` runs `claude -p` once per note with the vault root as cwd, `--tools Read,Glob,Grep,Edit --permission-mode acceptEdits --strict-mcp-config --setting-sources project`. Don't use `--bare`: it disables the OAuth login. The model is one of `haiku`, `sonnet`, `opus`, `fable` (validated server side, default `sonnet` in `Settings.ai_model`). Progress comes from `--output-format stream-json`. Claude writes `## Resumen` and `## Relacionado` between the author line and the first chapter, so appended quotes stay below them. `cargo test claude_enrich -- --ignored --nocapture` runs it with Haiku on a temp vault (~12 s).

**Anki cards (`anki.rs`).** `anki_start` runs `claude -p --output-format json --json-schema anki::SCHEMA --tools ""` once per book, from an empty dir in the app cache so no project `CLAUDE.md` loads. The cards come back in the result's `structured_output.cards`, each with `source` (1-based highlight number). Each card's GUID is `<block_id>-<n>`, so re-importing updates the same Anki notes; it depends on the same FNV hash as the Obsidian ids. The output is one tab-separated text file in `~/Downloads` with Anki's `#guid column`, `#deck column` and `#tags column` headers (docs.ankiweb.net/importing/text-files.html). `anki_open` runs `open -a Anki <file>`. `enrich_stop` also cancels this job. `cargo test claude_anki -- --ignored --nocapture` runs it with Haiku on the smallest cached book with 3+ highlights (~12 s).

**File browser (`files.rs`, `Files.svelte`).** Browses and changes anything under `/mnt/us`. Its guard: `under_us()` (no `.`, `..`, empty segment or newline), the top-level system folders (`documents`, `koreader`, `system`…) are never renamed, moved or deleted, and inside `koreader/` only `patches`, `plugins`, `fonts`, `styletweaks`, `screenshots`, `data` (and `settings` while `reader.lua` is not running) can change. Listings are `find | stat -c '%F|%s|%Y|%n'`. Batch commands print `OK|EXISTS|ERR|KEPT <path>` per item, never overwrite (`[ -e t ]` then `mv -T`), and append the new listing of each affected folder after an `@@KOB@@ <dir>` line, so a change is one ssh call and the UI applies the reply (no optimistic updates). A book's `X.sdr` (name minus its LAST extension) follows a rename or move unless it holds another format's `metadata.<ext>.lua`; delete purges only `metadata.<ext>.lua(.old)` and then `rmdir`s it. `fs_snapshot` lists all of `/mnt/us` (~5 s) once per connection into `files.json` and never replaces a folder already listed this session. Folder uploads are `ssh::upload_dir` (local `tar` piped into remote `tar -xf`). Internal moves use pointer events, because HTML5 drag doesn't work while Tauri's drop handler is on.

**Settings view and SSH setup (`setup.rs`, `Prefs.svelte`).** ⌘, (Ctrl+, on Linux) opens it through a `svelte:window` keydown handler in `App.svelte`. It holds the language and four SSH steps: create `~/.ssh/id_ed25519_kindle` with `ssh-keygen`, append the public key to `koreader/settings/SSH/authorized_keys` on a Kindle mounted over USB, put a `Host kindle` block at the top of `~/.ssh/config` (after copying it to `config.kob.bak`, refused if any `Host`/`Match originalhost` line already names `kindle`), and a first `ssh -o StrictHostKeyChecking=accept-new kindle` that saves the host key. The sidebar view that edits `defaults.custom.lua` and `settings.reader.lua` is labeled "KOReader files".

**USB link (`usb.rs`, `Cable.svelte`).** Puts the Kindle in MTP mode over the cable while KOReader runs, with the recipe of `~/Developer/kindlemtp.koplugin` (`kindlemtp_core.lua`, `docs/design.md`); keep both in step. `usb_status`, `usb_connect`, `usb_disconnect` and `usb_reset` are one `run_script` each and print a `key=value` block that `usb::parse` reads; a failed step comes back as `error=<code>`. Connect follows the plugin's `start()`: refuse a valid marker owned by someone else and refuse without `lipc-get-prop com.lab126.powerd isCharging` = 1, find the bus name (`com.lab126.mtp`, else the `:1.N` whose `GetConnectionUnixProcessID` is `pidof tizen-mtp`), claim `/tmp/kindlemtp.active` (`owner=kobridge pid= since=`, `set -C`; the marker is the lock, so a lost claim sends no device command), save `preventScreenSaver` to `/tmp/kindlemtp.pss` and set it to 1, `mtp.sh udc disable` + `stopMtp 1`, record `wc -l /var/log/messages`, `startMtp 1`, wait up to 20 s on the device for the `handled state_change_event [12] event [2]` line, then send `driveModeStateChanged` with `--dest=` (never a broadcast: it turns Wi-Fi off and draws the stock USB screen). Any failure after the claim tears down. Stop is `stopMtp 1`, `udc disable`, then restore `preventScreenSaver`, whoever owns the marker; reset is `restart mtp` then stop. A marker is stale only when `isMtpStarted` reads 0 and its `since` is at least 30 s old by the Kindle's `date +%s`; an unparsable or future `since` counts as fresh, and whoever reads a stale one removes it (Disconnect and Reset tear down regardless). `volumd` not in state `T` means the stock flow owns USB. Tests run the scripts against a fake device built from the plugin's fixtures (`src-tauri/src/fixtures/usb/`). On quit, `RunEvent::Exit` (not `ExitRequested`: Cmd-Q on macOS only sends `Exit`) releases a session KOBridge started, once, capped at 3 s; each script is also capped at 60 s. `facts` reads each lipc property once per script, since a read can block 10 s. The panel disconnects when our session loses power and the UDC isn't `configured`. The MTP app is `Settings.mtp_client` (chosen in Settings, opened by name with `open -a`) or the first of KindleMTP, OpenMTP, Android File Transfer in `/Applications` or `~/Applications` (macOS); `xdg-open mtp://` on Linux. `cargo test kindle_usb -- --ignored --nocapture` needs the cable.

**USB link safety rules.** From a reverse-engineering pass over stock `volumd` and `fsp`: MTP writes through the Kindle's own ext4 driver, so the risks are at the application level, not filesystem corruption. `fsp` reopens paths on every read, so KOReader sees files replaced under it: `usb_connect` refuses with `book_open` when any fd of `luajit`/`reader.lua` (`ls -l /proc/<pid>/fd`) points under `/mnt/us` outside `koreader/`, or into `koreader/cache/cr3cache/`. `tizen-mtp` never fsyncs, so teardown runs `sync` after `stopMtp 1` + `udc disable`. Interrupted transfers leave `/mnt/base-us/.mtp-tmp`, removed after teardown once `isMtpStarted` reads 0 and no valid marker remains. The cable counts when `isCharging` is 1, any `/sys/class/udc/*/state` isn't `not attached`, or any `/sys/class/power_supply/*/online` reads 1, because a full battery reads `isCharging` 0 with the cable in.

**Linux.** `.github/workflows/linux.yml` builds the `.deb` and `.AppImage` on Ubuntu 22.04 when a release is published and attaches them to it; run it by hand without a tag to only build. Platform differences live behind `cfg!(target_os = "macos")`: `UseKeychain=yes` is only passed on macOS (Linux ssh rejects it), `open_with` uses `open` or `xdg-open`, Obsidian's list is in `~/.config/obsidian/obsidian.json` on Linux, and Anki is launched as `anki <file>`.

**Frontend.**
- **Shared state.** `src/lib/store.svelte.ts` (Svelte 5 runes) holds global state: connection status polled every 30 s, settings and backup progress. It also runs the auto-backup: one attempt per day, and `last_auto_backup` is saved before the run so a failure doesn't retry every poll. `explain()` maps ssh stderr to a user-facing hint.
- **Translations.** Every UI string is inline: `t('English', 'Español')` from `src/lib/i18n.svelte.ts`. `t` reads a `$state`, so text updates when the language changes; a list built from `t` must be `$derived`. Use `dec()` for decimals. Text written into the vault or sent to Claude (note headings, prompts) stays Spanish, because `merge()` matches those headings.
- **Typed wrappers.** `src/lib/api.ts` has the typed `invoke` wrappers and the formatters, which follow the UI language (decimal comma and Spanish dates in Spanish).
- **Views stay mounted.** `App.svelte` mounts a view the first time it is opened and then only toggles `hidden`, so switching is instant and keeps scroll and selection. Stats and Highlights are mounted hidden at launch to prefetch. They load their local cache first, then fetch once through an `$effect` that waits for `app.status`, and show `Loading.svelte` while empty. Log is the exception and remounts on each visit, because it streams `tail -f`. Its lines are appended once per animation frame.
- **Uploads.** `Dropzone.svelte` (Tauri `onDragDropEvent` plus the dialog plugin, sequential queue) serves both Send books and Fonts. Both stay mounted, so a drop is ignored unless the zone is visible (`checkVisibility()`).

## Design rules

- **Tokens.** All colors are tokens in `src/app.css`, with light values and dark values under `prefers-color-scheme`. The palette is e-ink greys and ink-black actions. The only color, highlighter yellow `--mark`, is reserved for highlighted text.
- **Fonts.** System fonts only: `-apple-system` for UI, `ui-serif` (New York) for book titles and quotes. No web fonts or icon/CSS libraries; icons are inline Lucide path strings in `App.svelte`.
- **No accent rails.** The user rejects them as AI slop: no `border-left` or inset-shadow bars on selected items or quotes. Selection is a background change.
- **No inert waits.** Every async action shows feedback (button label change, spinner, or `Loading`).
- **Window.** The window starts with `visible: false` and `App.svelte` calls `show()` on mount, so it never flashes an unstyled frame. `titleBarStyle: Overlay` with `.titlebar` drag regions. Only `.pane` scrolls (`html`/`body` are `overflow: hidden`).

## Release / install

- **Local Network permission.** `src-tauri/Info.plist` adds `NSLocalNetworkUsageDescription`. Without it, macOS may silently block the ssh child from reaching the LAN when the app is launched from Finder, even though the same command works from Terminal.
- **Installing.** Copy `src-tauri/target/release/bundle/macos/KOBridge.app` over `/Applications/KOBridge.app`.
- **Icon.** Source is `assets/icon.svg`; regenerate with `rsvg-convert -w 1024 -h 1024 assets/icon.svg -o assets/icon.png && bunx tauri icon assets/icon.png`, then delete `src-tauri/icons/{android,ios}`.

## Planned

A read-only Calibre library view (read `metadata.db`, compare with the Kindle, send books, and register sent books in the device's `.metadata.calibre` so Calibre sees them) is planned inside KOBridge with its own spec. A Calibre device plugin over SSH is a separate project.
