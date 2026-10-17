# MEMORY.md

Decisions and lessons from building KOBridge and kindlemtp.koplugin that the code and CLAUDE.md don't explain. Newest first.

## 2026-10-17

- In-app updates use `tauri-plugin-updater` with a static `latest.json` on the GitHub release, because the Mac build is made on this Mac and the Linux build in Actions, and both have to land in one manifest. The Mac release uploads `latest.json` with the `darwin-aarch64-app` entry, and `linux.yml` adds the Linux entries, so `latest.json` must be on the release before it is published.
- The update installs only after a confirmation dialog, because the restart kills running uploads, file changes and Claude jobs. A backup won't start during an update download. Both came from a code review before 0.6.0.
- macOS builds are signed with the self-signed certificate "KOBridge Self-Signed" since 0.6.1. Ad-hoc builds got a new identity per build, so macOS dropped the Local Network permission on every update and the app showed "Kindle offline" while Terminal still reached the Kindle. A paid Developer ID would also remove the Gatekeeper step, and the user hasn't asked for one.
- The two signing keys live in `~/.tauri/` (updater key `kobridge.key`, code signing `kobridge-codesign.p12`). Losing the updater key ends in-place updates for every install, so the user was told to back both up.
- Firmware 5.19.x breaks the USB link (see CLAUDE.md, USB link). Reverse engineering needs the 5.19 binaries, and the user is collecting them from Reddit testers with `kindlemtp.koplugin/tools/collect-519.sh` over SSH. A Scribe user promised a PR on 2026-10-16, and none has arrived.

## 2026-10-15 to 2026-10-16

- The USB link copies the kindlemtp.koplugin recipe instead of calling the plugin, so KOBridge works without the plugin installed. The two share the `/tmp/kindlemtp.active` marker as a lock, so any change to the recipe goes to both repos in the same session.
- The cable check counts `Mains` and `USB*` power supplies but not the battery or the `USB_WET` moisture sensor, because those always read `online` 1 on the Paperwhite. A full battery reads `isCharging` 0 with the cable in, which caused GitHub issue #1 in the plugin (fixed in v1.0.2).
- MTP doesn't corrupt the userstore the way USB mass storage does, because `tizen-mtp` writes through the Kindle's own ext4 driver. The real risks are KOReader reading a file the computer replaced, unsynced writes and leftovers from interrupted copies, which the refuse-with-a-book-open rule, `sync` and the `.mtp-tmp` cleanup cover.
