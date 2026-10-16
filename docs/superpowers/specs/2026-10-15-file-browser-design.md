# File browser design (2026-10-15)

A sidebar view "Files" that browses and manages the whole `/mnt/us` over the existing SSH transport, with Finder-like interaction. It replaces the wish for a native OS mount: macOS cannot mount SFTP without macFUSE/FUSE-T, and a local WebDAV bridge was estimated at 3 to 5 days with Finder quirks (read-only without LOCK, `.DS_Store`/`._` writes, ~10 s PROPFIND storms on a 200-book folder).

Reviewed by Fable on 2026-10-15; its changes are folded in below.

## Goals

- Browse every folder under `/mnt/us`, and rename, move, delete and create folders.
- Upload files and folders by dragging them from Finder onto the window or onto a folder row, into any folder (books, `koreader/`, screensavers, dictionaries).
- Feel instant: folders already seen render from memory; each change answers in one ssh round trip (~350 ms).

Out of scope: dragging out to Finder (needs a new plugin; "Download to…" covers it), Quick Look, a Trash on the Kindle, the USB link (own spec).

## Device facts (measured 2026-10-15, BusyBox 1.34.1, warm mux)

- ssh round trip 260–380 ms. A folder of 494 entries lists in 350 ms, 1,792 entries in 635 ms.
- Full `find|stat` of `/mnt/us`: 5.0 s, 15,120 entries, 2.34 MB.
- `find -printf` is unsupported. `mv` supports `-T` and `-n`.
- `/mnt/us` (fsp FUSE over ext4) is case-sensitive and has no symlinks. 18 files print `regular empty file`.

## KOReader sidecars

KOReader keeps progress and highlights in `<stem>.sdr/` next to the book, where `<stem>` is the name without its LAST extension (`docsettings.lua`). `X.epub` and `X.pdf` therefore share `X.sdr`, which holds `metadata.epub.lua` and `metadata.pdf.lua`.

- Pairing: only a regular file with a dot in its name pairs with a directory `<stem>.sdr` in the same folder.
- Rename and move carry the whole `X.sdr` only when it holds no `metadata.<other ext>.lua`. Otherwise the sidecar stays, and the UI says so.
- Delete mirrors KOReader's purge: remove `X.sdr/metadata.<ext>.lua` and `metadata.<ext>.lua.old`, then `rmdir X.sdr 2>/dev/null`.
- If both `X.epub` and `X.sdr` are selected, the `.sdr` is handled once, as the book's sidecar.
- Default: `.sdr` folders are hidden. The toolbar toggle "Show KOReader data folders" shows them; saved as `Settings.show_sdr: bool` (default `false`). The carry rules apply either way.

## Backend: `src-tauri/src/files.rs`

| Command | Remote command | Notes |
|---|---|---|
| `fs_list(dir)` | `find <dir> -mindepth 1 -maxdepth 1 -name '.?*' -prune -o -exec stat -c '%F\|%s\|%Y\|%n' {} + 2>/dev/null; true` | Primary path. Hidden dot entries are skipped. |
| `fs_snapshot()` | same `find` over all of `/mnt/us`, grouped by dir | Background warm-up once per connection. Fills only dirs not already in the map. Cached as `files.json` in `app_cache_dir()`. |
| `fs_rename(from, to)` | per item: `[ -e t ] && echo "EXISTS s" \|\| { mv -T s t && echo "OK s"; }`, plus the sidecar | Never overwrites. |
| `fs_move(paths, dir)` | same per-item guard, one ssh call | |
| `fs_delete(paths)` | `rm -rf` the item, sidecar purge as above | After a native confirm dialog. |
| `fs_mkdir(path)` | `mkdir` (fails if it exists) | |
| `fs_upload(path, dir)` | file: `ssh::upload`. Folder: local `COPYFILE_DISABLE=1 tar -cf - -C parent name` piped into remote `tar -xf - -C dir` | New primitive `ssh::upload_dir` next to `upload`. |
| `fs_download(path, dest)` | `ssh::pull_to_file` (`cat` for files, `tar -cf - -C parent name` for folders, saved as `.tar`) | |

Parsing `stat` lines: `splitn(4, '|')` with the name last. Type by prefix: `directory`, `symbolic link`, anything else is a file. A line without a known type prefix is glued onto the previous name (a name with a newline), and `under_us` then rejects that entry for any action.

Batch commands print `OK <path>` / `EXISTS <path>` / `ERR <path>` per item, so the UI shows exactly which items failed.

### Guards

- `under_us(path)`: `/mnt/us` or starts with `/mnt/us/`; no `.`, `..` or empty segment; no newline.
- Never deleted, renamed or moved: `/mnt/us`, and the top-level `documents`, `audible`, `kpm`, `voice`, `libkh`, `mrpackages`, `system`, `extensions`, `koreader`.
- Inside `koreader/`, only these subtrees are writable: `patches`, `plugins`, `fonts`, `styletweaks`, `screenshots`, `data`. Everything else there is read-only (listing and download only).
- `koreader/settings/*` changes are refused while `reader.lua` runs (`reader_running()`, same check as `write_remote`).
- Every value goes through `ssh::sq()`. Error strings use `ssh::tr(en, es)`.

## Frontend

- `src/lib/views/Files.svelte`, sidebar entry "Files"/"Archivos" with a Lucide folder icon. Stays mounted once opened; not prefetched at launch.
- `src/lib/files.svelte.ts` holds the map dir → entries, the current dir, selection and sort.
- List with Name, Size, Modified columns (sortable), clickable path bar, toolbar: back, new folder, upload, refresh, sidecar toggle. Free space from `app.status.free_kb`.
- Selection: click, ⌘-click, ⇧-click. Keys: arrows, Enter renames inline, ⌘⌫ deletes, ⌘↑ goes up, ⌘↓ or double-click opens a folder, ⌘⇧N new folder, type-ahead jumps to a name.
- Right-click: native menu (`@tauri-apps/api/menu`): Open, Rename, Download to…, New Folder, Delete. Read-only and protected items show those entries disabled.
- Finder drop: Tauri `onDragDropEvent`. Its `position` is passed to `document.elementFromPoint` as is (macOS gives points, not physical pixels; confirm once on Retina). A folder row under the pointer receives the upload; elsewhere the current folder does. Drop target gets a background change (no accent rails). Uploads run one at a time with a progress strip.
- Internal move: HTML5 drag doesn't work in the webview while Tauri's native drop handler is on (tauri-runtime-wry, issue #14373). Use pointer events instead: pointerdown, start after 4 px, `setPointerCapture`, `elementFromPoint` on move to highlight a folder row or path segment, act on pointerup.
- No optimistic updates. A pending row is dimmed, and the reply updates the map. After a folder rename or move, the map drops that dir's descendant keys, which are refetched when opened.
- The snapshot never overwrites a dir already in the map, so a snapshot started before a change can't revert it.
- Performance: plain keyed rows with `content-visibility: auto; contain-intrinsic-size: auto 28px`; the sorted list is `$derived`.

## Capabilities

`core:default` already allows the menu popup. Add `dialog:allow-save` and `dialog:allow-ask`.

## Testing

- `cargo test`: `under_us` (rejects `..`, `/mnt/usx`, newline, empty segment), protected and read-only paths, `stat` parsing (types by prefix, `|` in names, glued newline names), sidecar pairing (shared `X.sdr`, no-dot names, dedupe when both are selected).
- Live `#[ignore]` test: mkdir, upload a file and a folder, rename with `.sdr`, move, refuse an overwrite, delete, under `/mnt/us/kob test/`, then clean up.
- `bun run check` at 0 errors / 0 warnings.
