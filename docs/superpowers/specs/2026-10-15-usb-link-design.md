# USB link design (2026-10-15)

Let KOBridge put the Kindle in MTP mode over the cable while KOReader runs, and open the computer's MTP client on it. Companion to the KOReader plugin `kindlemtp.koplugin` (`~/Developer/kindlemtp.koplugin/docs/design.md`), which does the same from the device.

## Findings (2026-10-15, Paperwhite 5.18.5, kernel 5.15.41-lab126)

| Option | Verdict | Why |
|---|---|---|
| USB mass storage of the userstore | Not viable | `/mnt/base-us` is an ext4 image (`/dev/loop/0`) mounted read-write under the `fsp` FUSE layer; a second writer corrupts it, and macOS can't read ext4. |
| Mass storage of a separate FAT32 image | Possible, not planned | Kernel has `usb_f_mass_storage`; works as a drop box synced after unplug. Not live, case clashes. Fallback only. |
| USB networking (`g_ether` / USBNetLite) | Possible, not planned | Needs the same single UDC (`11211000.usb`) as MTP; USBNetLite's dropbear is broken on 5.18.x; needs a manual IP on the Mac. |
| MTP with `tizen-mtp` | **Chosen** | Works with KOReader running, Wi-Fi up, no stock screen. Verified: 3,833 folders listed through libmtp. |

Recipe, why the signal must be unicast, the bus-name lookup and the undo are in the plugin design and in project memory `kindle-mtp-under-koreader`.

## Goals

- One action in KOBridge: "Connect by cable" → the Kindle appears to the computer's MTP client.
- One action to disconnect, and automatic cleanup if the cable is pulled or the app quits.
- Tell the user exactly what's missing when it can't work (cable, unsupported device, `volumd` running, no MTP client installed).

Out of scope: a Finder mount (macOS has no MTP support), mass storage, USB networking.

## Where it lives

- Backend: `src-tauri/src/usb.rs`, commands `usb_status`, `usb_connect`, `usb_disconnect`, `usb_reset`.
- Frontend: a "Cable" panel at the top of the Files view (`Files.svelte`), since Files is where the user manages the Kindle's files. It shows state, the Connect/Disconnect button and the client that will open.

## Backend

Every command is one `ssh::run_script` call (script on stdin, under dropbear's 9,000-byte exec limit by construction).

| Command | Does | Returns |
|---|---|---|
| `usb_status()` | Probe (below) plus current state: UDC file, `isMtpStarted`, `/sys/class/udc/*/state`, marker `/tmp/kindlemtp.active` | `UsbStatus { supported, reason, cable, state }` |
| `usb_connect()` | Probe; refuse another owner's valid marker and no power; look up `tizen-mtp`'s bus name; claim the marker (`set -C`, the lock, before touching the responder); save `preventScreenSaver` to `/tmp/kindlemtp.pss` and set it to 1; reset; record the syslog line count; `startMtp 1`; wait up to 20 s on the device for the host to configure (rule from the plugin design's "Host detection"); unicast drive-mode signal. A lost claim (`marker_failed`) sends no device command and tears nothing down; any failure after the claim tears down | `UsbStatus` |
| `usb_disconnect()` | `stopMtp 1`; `mtp.sh udc disable`; restore `preventScreenSaver` from `/tmp/kindlemtp.pss`; remove the marker | `UsbStatus` |
| `usb_reset()` | `restart mtp`, then disconnect | `UsbStatus` |

- The wait in `usb_connect` runs on the device inside the script (a loop polling every 250 ms for up to 20 s), so it is one ssh call. Host detection, destination and marker follow the plugin design verbatim: syslog line `handled state_change_event [12] event [2] current mtp state` after the recorded line count; `--dest=com.lab126.mtp` with the `:1.N` scan as fallback; marker `/tmp/kindlemtp.active` with `owner=kobridge`.
- `usb_connect` refuses when a valid marker has `owner=plugin`; `usb_disconnect` always tears down.
- Marker staleness (same rule as the plugin): a marker is stale only when `isMtpStarted` reads 0 **and** its `since` is at least 30 s old by the Kindle's clock (`date +%s` on the device; the plugin writes `since` with `os.time()` there). An unparsable or future `since` counts as fresh and is never removed by the other side; Disconnect and Reset still tear down. This covers the window between a fresh claim and its `startMtp`, when `isMtpStarted` still reads 0. A failed `isMtpStarted` read keeps the marker.
- `cable`, labeled "charger or computer": see the Cable rule below. `isCharging` alone missed a full battery.
- The recipe is duplicated here, not called from the plugin: it is six commands plus one grep, the plugin is optional, and a device-resident script would add version skew KOBridge can't pin. Fixture files and the rule text are copied from the plugin repo; if the recipe grows, the plugin ships `kindlemtp.sh` and KOBridge vendors it with a hash test.
- Probe (same checks as the plugin): `/usr/bin/mtp.sh`, the configfs `mtpgadget`, `pidof tizen-mtp`, `/usr/bin/dbus-send`, `lipc-get-prop -eiq com.lab126.mtp isMtpStarted`, and `volumd` in state `T` (`/proc/<pid>/stat`). A running `volumd` means the stock flow already handles USB: the panel says so and the button is hidden.
- Book open: KOBridge can't see KOReader's UI state. The panel warns "Close the book on the Kindle first", and if `kindlemtp.koplugin` is installed (`/mnt/us/koreader/plugins/kindlemtp.koplugin`), suggests starting from the Kindle instead, where the plugin enforces it.
- Errors use `tr(en, es)` and name the failing step.
- Safety rules (from reverse-engineering stock `volumd`/`fsp`, same as the plugin): MTP writes through the Kindle's own ext4 driver, so the risks are application-level. `fsp` reopens paths on every read, so KOReader sees files replaced under it: connect refuses with `book_open` when any fd of `luajit`/`reader.lua` points under `/mnt/us` outside `koreader/` or into `koreader/cache/cr3cache/`. `tizen-mtp` never fsyncs, so teardown runs `sync` after `stopMtp 1` and `udc disable`. Interrupted transfers leave `/mnt/base-us/.mtp-tmp`, removed after teardown when `isMtpStarted` reads 0 and no valid marker remains.
- Cable: `isCharging` 1, or any `/sys/class/udc/*/state` other than `not attached`, or any `/sys/class/power_supply/*/online` reading 1 (a full battery reads `isCharging` 0 with the cable in).
- Wi-Fi: the unicast signal keeps Wi-Fi up, so the SSH session that started MTP can also stop it.

## Computer side

| OS | Client | How KOBridge opens it |
|---|---|---|
| macOS | KindleMTP.app (`~/Developer/KindleMTP`, libmtp) if installed, else OpenMTP, else Android File Transfer | `open -a <app>` after connect |
| Linux | the file manager through gvfs/kio | `xdg-open mtp://` (gvfs shows the device by name) |

- macOS's `ptpcamerad` claims MTP devices as soon as they appear and blocks libmtp clients (`libusb_claim_interface() = -3`). The panel shows a hint if the client reports the device busy. KOBridge does not kill system daemons on its own.
- The chosen client is saved as `Settings.mtp_client` (empty = auto-detect in the order above).

## Frontend

- Panel states: Unsupported (reason) · No cable · Ready (Connect) · Connecting… (spinner, up to 20 s) · Connected (Disconnect, "Open <client>") · Error (text, Retry, Reset).
- Polls `usb_status` every 5 s only while the Files view is visible and the panel isn't idle-unsupported.
- On app quit (`tauri::RunEvent::Exit`, which fires for both Cmd-Q, where macOS goes through `applicationWillTerminate` and never sends `ExitRequested`, and the last window closing): if the marker says KOBridge started MTP, run `usb_disconnect` (best effort, 3 s cap, once).
- Cable pulled: when a status shows our session with no power and the UDC state isn't `configured`, the panel disconnects.
- After Disconnect, the Files view refreshes the current folder.

## Testing

- `cargo test`: the generated scripts pass `sh -n`; status parsing from captured device outputs (all probe failures, cable states, marker); the drive-mode line always has `--dest=` (never a broadcast); bus-name parsing fixtures shared with the plugin.
- Live `#[ignore]` test `kindle_usb`: needs the cable; connect, assert state Connected and Wi-Fi still answering, disconnect, assert UDC empty.
- `bun run check` at 0 errors.

## Order of work

1. Device checks still open (both specs): host-detection rule; whether `tizen-mtp` owns a well-known bus name; `VOLUMD_STOPPED` in KOReader's environment; fsp seeing MTP writes immediately.
2. The plugin first (it carries the shared knowledge and the busted fixtures).
3. Then `usb.rs` and the panel, reusing the plugin's fixtures.
