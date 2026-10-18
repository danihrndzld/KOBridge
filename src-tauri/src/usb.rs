//! USB link: puts the Kindle in MTP mode over the cable while KOReader runs, the same recipe
//! as kindlemtp.koplugin (`kindlemtp_core.lua`, docs/design.md). Every command is one
//! `run_script` call; the script prints a `key=value` block that `parse` reads.
//! Fixtures in `fixtures/usb/` are copied from the plugin's `spec/fixtures/`.

use crate::ssh::{self, tr, Res};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use tauri::AppHandle;

/// Set when a status showed KOBridge's marker, so quitting only spends an ssh call when needed.
static OURS: AtomicBool = AtomicBool::new(false);

/// Device paths; tests swap this block for a fake device.
const PATHS: &str = "MTP_SH=/usr/bin/mtp.sh
DBUS=/usr/bin/dbus-send
GADGET=/sys/kernel/config/usb_gadget/mtpgadget
LOG=/var/log/messages
MARKER=/tmp/kindlemtp.active
PSS=/tmp/kindlemtp.pss
PLUGIN=/mnt/us/koreader/plugins/kindlemtp.koplugin
UDCS=/sys/class/udc
POWER=/sys/class/power_supply
BASEUS=/mnt/base-us
PROC=/proc
IPCF=/tmp/kindlemtp-ipcf
WAIT=20
";

/// Shell helpers. The sed snippets mirror the plugin's parse_names, parse_pid, first_pid and proc_state.
const LIB: &str = r#"STATE2='handled state_change_event [12] event [2] current mtp state'
get() { lipc-get-prop -eiq "$1" "$2" 2>/dev/null; }
first() { sed -n '1s/^[[:space:]]*\([0-9][0-9]*\).*/\1/p'; }
names() { sed -n 's/^[[:space:]]*string "\([^"]*\)"[[:space:]]*$/\1/p'; }
pid_of() { sed -n 's/^[[:space:]]*uint32 \([0-9][0-9]*\)[[:space:]]*$/\1/p' | head -n 1; }
pstate() { sed -n 's/^[0-9][0-9]* (.*) \([A-Za-z]\) .*$/\1/p'; }
# Firmware 5.19 on: tizen-mtp registers no lipc properties; $IPCF (kindlemtp.koplugin's ipcf/) calls
# its IPCF4 service, which sets the same flag and sends the same events as startMtp/stopMtp 1.
# Its ping prints no_ipcn on older firmware. Nothing reports the state there, so the gadget's
# UDC binding stands in for isMtpStarted.
mtp() {
  if [ -n "$ipcf" ]; then "$IPCF" "$1" >/dev/null 2>&1
  else lipc-set-prop -i com.lab126.mtp "${1}Mtp" 1 >/dev/null 2>&1
  fi
}
started() {
  if [ -n "$ipcf" ]; then [ -n "$(cat "$GADGET/UDC" 2>/dev/null)" ] && echo 1 || echo 0
  else get com.lab126.mtp isMtpStarted
  fi
}
facts() {
  if [ -z "$ctl" ]; then
    # the plugin's copy when it is installed (in case /tmp is noexec), else the one KOBridge uploads
    [ -x "$PLUGIN/bin/kindlemtp-ipcf" ] && IPCF="$PLUGIN/bin/kindlemtp-ipcf"
    ctl=lipc; ipcf=; "$IPCF" ping >/dev/null 2>&1 && { ctl=ipcf; ipcf=1; }
  fi
  s=$(started)
  m=$(cat "$MARKER" 2>/dev/null)
  # stale only when started reads an explicit 0 AND since is 30 s old or more by the Kindle's clock;
  # a failed read, a fresh claim (before its startMtp), an unparsable or a future since all keep it
  if [ -n "$m" ] && [ "$s" = 0 ]; then
    t0=$(echo "$m" | sed -n 's/^owner=[^ ][^ ]* pid=[0-9][0-9]* since=\([0-9][0-9]*\)$/\1/p')
    if [ -n "$t0" ] && [ $(( $(date +%s) - t0 )) -ge 30 ]; then rm -f "$MARKER"; m=; fi
  fi
  r=$(pidof tizen-mtp | first)
  v=$(pidof volumd | first); vs=
  [ -n "$v" ] && vs=$({ pstate < "$PROC/$v/stat"; } 2>/dev/null)
  c=$(get com.lab126.powerd isCharging)
  # isCharging alone reads 0 on a full battery with the cable in: any UDC state other than
  # "not attached" or an external supply online also counts as a cable (charger or computer).
  # Only Mains and USB* supplies: the battery and USB_WET (moisture) always read online=1.
  cab=; [ "$c" = 1 ] && cab=1
  for f in "$UDCS"/*/state; do [ -f "$f" ] && [ "$(cat "$f")" != "not attached" ] && cab=1; done
  for d in "$POWER"/*; do
    case "$(cat "$d/type" 2>/dev/null)" in
      USB_WET) ;;
      Mains|USB*) [ "$(cat "$d/online" 2>/dev/null)" = 1 ] && cab=1 ;;
    esac
  done
}
# Prints y when KOReader holds a file open under /mnt/us outside koreader/, or in cr3cache:
# a book is open, and KOReader would rewrite its sidecar over changes made from the computer.
bookopen() {
  for p in $(pidof luajit reader.lua); do ls -l "$PROC/$p/fd" 2>/dev/null | sed -n 's/.* -> //p'; done | while read -r f; do
    case "$f" in
      /mnt/us/koreader/cache/cr3cache/*) echo y; break ;;
      /mnt/us/koreader/*) ;;
      /mnt/us/*) echo y; break ;;
    esac
  done
}
reason() {
  if [ ! -e "$MTP_SH" ]; then echo no_mtp_sh
  elif [ ! -e "$GADGET" ]; then echo no_gadget
  elif [ -z "$r" ]; then echo no_responder
  elif [ ! -e "$DBUS" ]; then echo no_dbus_send
  elif [ -z "$s" ]; then echo no_lipc
  elif [ -n "$vs" ] && [ "$vs" != T ]; then echo volumd_running
  fi
}
st() {
  echo "reason=$(reason)"
  echo "started=$s"
  echo "control=$ctl"
  echo "charging=$c"
  echo "cable=$cab"
  echo "udc=$(cat "$GADGET/UDC" 2>/dev/null)"
  echo "udc_state=$(cat "$UDCS"/*/state 2>/dev/null | head -n 1)"
  echo "marker=$m"
  echo "plugin=$([ -d "$PLUGIN" ] && echo 1)"
}
teardown() {
  mtp stop
  # stopMtp from state 3 does not unbind the UDC
  "$MTP_SH" udc disable >/dev/null 2>&1
  # tizen-mtp never fsyncs what it wrote
  sync
  if [ -f "$PSS" ]; then
    lipc-set-prop -i com.lab126.powerd preventScreenSaver "$(cat "$PSS")" >/dev/null 2>&1
    rm -f "$PSS"
  fi
  rm -f "$MARKER"
  facts
  # what an interrupted transfer left behind, once nobody holds MTP
  if [ "$s" = 0 ] && [ -z "$m" ]; then rm -rf "$BASEUS/.mtp-tmp"; fi
}
busname() {
  l=$("$DBUS" --system --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.ListNames 2>/dev/null) || return 0
  if echo "$l" | names | grep -qx com.lab126.mtp; then echo com.lab126.mtp; return 0; fi
  p=$(pidof tizen-mtp | first)
  [ -n "$p" ] || return 0
  for n in $(echo "$l" | names | grep '^:1\.[0-9][0-9]*$'); do
    q=$("$DBUS" --system --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.GetConnectionUnixProcessID "string:$n" 2>/dev/null | pid_of)
    if [ "$q" = "$p" ]; then echo "$n"; return 0; fi
  done
}
waithost() {
  end=$(( $(date +%s) + WAIT )); seen=; i=0
  while :; do
    i=$(( i + 1 ))
    n=$(wc -l < "$LOG" 2>/dev/null | tr -d ' ')
    if [ -z "$base" ]; then base=$n
    elif [ -n "$n" ] && [ "$n" -lt "$base" ]; then base=0
    fi
    tail -n +$(( ${base:-0} + 1 )) "$LOG" 2>/dev/null | grep -qF "$STATE2" && return 0
    # secondary hint: the responder unbinds the gadget at state 2
    if [ -n "$(cat "$GADGET/UDC" 2>/dev/null)" ]; then seen=1
    # a lipc read can block for 10 s, so about once a second
    elif [ -n "$seen" ] && [ $(( i % 4 )) = 0 ] && [ "$(started)" = 1 ]; then return 0
    fi
    [ "$(date +%s)" -ge "$end" ] && return 1
    usleep 250000 2>/dev/null || sleep 1
  done
}
"#;

/// Start, as `start()` + `poll()` in the plugin: refusals, bus name, claim the marker, keep-awake,
/// reset, startMtp, wait for the host, then the unicast drive-mode signal. A failure after the claim tears down.
const CONNECT: &str = r#"err=
if [ -n "$(reason)" ]; then :
elif [ -n "$m" ]; then
  case "$m" in
    owner=kobridge\ *) ;;
    owner=plugin\ *) err=owner_plugin ;;
    *) err=owner_other ;;
  esac
elif [ "$cab" != 1 ]; then err=no_power
elif [ -n "$(bookopen)" ]; then err=book_open
else
  dest=$(busname)
  if [ -z "$dest" ]; then err=bus_name_not_found
  else
    # the marker is the lock: claim it before touching the responder
    if ! (set -C; printf 'owner=kobridge pid=%s since=%s\n' $$ "$(date +%s)" > "$MARKER") 2>/dev/null; then err=marker_failed
    else
      # keep the Kindle awake during the transfer; teardown restores the saved value
      k=$(get com.lab126.powerd preventScreenSaver)
      echo "${k:-0}" > "$PSS"
      lipc-set-prop -i com.lab126.powerd preventScreenSaver 1 >/dev/null 2>&1
      "$MTP_SH" udc disable >/dev/null 2>&1
      mtp stop
      base=$(wc -l < "$LOG" 2>/dev/null | tr -d ' ')
      if ! mtp start; then err=startmtp_failed
      elif ! waithost; then err=no_host
      elif ! "$DBUS" --system --dest="$dest" --type=signal /default com.lab126.volumd.driveModeStateChanged int32:1 >/dev/null 2>&1; then err=signal_failed
      elif [ -n "$ipcf" ]; then
        # 5.19: started is the UDC binding, which the responder restores (state 2 -> 3) a moment
        # after the signal; read too early, a working connection would report idle
        j=0; while [ -z "$(cat "$GADGET/UDC" 2>/dev/null)" ] && [ $j -lt 12 ]; do usleep 250000 2>/dev/null || sleep 1; j=$(( j + 1 )); done
      fi
    fi
  fi
  # a lost claim (marker_failed) touched nothing; after the claim every failure tears down
  case "$err" in startmtp_failed|no_host|signal_failed) teardown ;; *) facts ;; esac
fi"#;

const STATUS: &str = "err=";
/// Stop always tears down, whoever owns the marker.
const DISCONNECT: &str = "err=; teardown";
const RESET: &str = "err=; restart mtp >/dev/null 2>&1 || err=restart_failed; teardown";
/// On quit: only a session KOBridge started.
const RELEASE: &str = "err=; case \"$m\" in owner=kobridge\\ *) teardown ;; esac";

/// `facts` runs once, and again only after the body changed something (teardown, connect).
/// The body runs inside a group fed /dev/null, so no command can eat the rest of the script from stdin.
fn script(body: &str) -> String {
    format!("{PATHS}{LIB}{{\nfacts\n{body}\nst\necho \"error=$err\"\n}} </dev/null\nexit 0\n")
}

#[derive(Serialize, Debug, PartialEq, Default)]
pub struct UsbStatus {
    pub supported: bool,
    /// probe failure code (`volumd_running`, `no_responder`, ...), empty when supported
    pub code: String,
    pub reason: String,
    /// USB power: charger or computer
    pub cable: bool,
    /// `idle`, `connected` (KOBridge's marker) or `busy` (the plugin's or another owner's)
    pub state: String,
    /// kindlemtp.koplugin is installed
    pub plugin: bool,
    pub udc: String,
    /// `/sys/class/udc/*/state`: `configured` while a computer holds the link
    pub udc_state: String,
    /// computer-side app opened after connect, None when none is installed
    pub client: Option<String>,
}

fn probe_reason(code: &str) -> String {
    match code {
        "no_mtp_sh" => tr("This Kindle has no /usr/bin/mtp.sh, so it doesn't use MTP over the cable.", "Esta Kindle no tiene /usr/bin/mtp.sh, así que no usa MTP por cable."),
        "no_gadget" => tr("The MTP USB gadget (mtpgadget in configfs) is missing.", "Falta el gadget USB de MTP (mtpgadget en configfs)."),
        "no_responder" => tr("The MTP responder (tizen-mtp) isn't running. Reset it, or restart the Kindle.", "El responder MTP (tizen-mtp) no está corriendo. Reinicialo o reiniciá la Kindle."),
        "no_dbus_send" => tr("/usr/bin/dbus-send is missing.", "Falta /usr/bin/dbus-send."),
        "no_lipc" => tr("The MTP responder doesn't answer, neither over lipc (com.lab126.mtp) nor over IPCF4 (com.lab126.mtp.service).",
                        "El responder MTP no contesta, ni por lipc (com.lab126.mtp) ni por IPCF4 (com.lab126.mtp.service)."),
        "volumd_running" => tr("volumd is running, so the Kindle's own USB mode handles the cable. Plug it in and use the Kindle's screen.",
                               "volumd está corriendo, así que el modo USB propio de la Kindle maneja el cable. Conectalo y usá la pantalla de la Kindle."),
        _ => tr(format!("Unsupported: {code}"), format!("No compatible: {code}")),
    }
}

fn step_error(code: &str) -> String {
    match code {
        "owner_plugin" => tr("The KOReader plugin is running a USB transfer. Stop it on the Kindle first.", "El plugin de KOReader está haciendo una transferencia USB. Detenela primero en la Kindle."),
        "owner_other" => tr("Another program started USB transfer on the Kindle. Disconnect it first.", "Otro programa inició la transferencia USB en la Kindle. Desconectala primero."),
        "book_open" => tr("Close the book on the Kindle first: KOReader rewrites its sidecar and would overwrite changes made from the computer.",
                          "Cerrá el libro en la Kindle primero: KOReader reescribe su sidecar y pisaría los cambios hechos desde la computadora."),
        "no_power" => tr("The Kindle gets no power over USB. Connect the cable to this computer.", "La Kindle no recibe energía por USB. Conectá el cable a esta computadora."),
        "bus_name_not_found" => tr("Bus name: tizen-mtp has no D-Bus name, so drive mode can't be sent. Try Reset.", "Nombre de bus: tizen-mtp no tiene nombre en D-Bus, así que no se puede enviar el modo unidad. Probá Reiniciar."),
        "startmtp_failed" => tr("startMtp: the Kindle refused to start MTP.", "startMtp: la Kindle se negó a iniciar MTP."),
        "marker_failed" => tr("Marker: /tmp/kindlemtp.active already exists, another transfer started at the same time.", "Marcador: /tmp/kindlemtp.active ya existe, otra transferencia empezó al mismo tiempo."),
        "no_host" => tr("Host: no computer set up the USB link within 20 s. Use a data cable plugged into this computer, not a charger.",
                        "Host: ninguna computadora configuró el enlace USB en 20 s. Usá un cable de datos conectado a esta computadora, no a un cargador."),
        "signal_failed" => tr("Drive mode: dbus-send failed.", "Modo unidad: dbus-send falló."),
        "restart_failed" => tr("restart mtp failed; the responder was still stopped.", "restart mtp falló; igual detuve el responder."),
        _ => tr(format!("USB step failed: {code}"), format!("Falló un paso USB: {code}")),
    }
}

/// `owner` of a well-formed `owner=<o> pid=<n> since=<epoch>` marker.
fn owner(m: &str) -> Option<&str> {
    let mut it = m.split(' ');
    let o = it.next()?.strip_prefix("owner=").filter(|o| !o.is_empty())?;
    let num = |p: Option<&str>, k: &str| p.and_then(|x| x.strip_prefix(k)).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    (num(it.next(), "pid=") && num(it.next(), "since=") && it.next().is_none()).then_some(o)
}

/// The script's `key=value` block; a failed step is an `Err` naming it.
fn parse(out: &str) -> Res<UsbStatus> {
    let get = |k: &str| out.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).unwrap_or("").trim();
    if !get("error").is_empty() { return Err(step_error(get("error"))) }
    let code = get("reason");
    let marker = get("marker");
    // facts already dropped stale markers, so any marker left is valid or a fresh claim
    let state = match (marker.is_empty(), get("started") == "1", owner(marker)) {
        (true, ..) => "idle",
        (false, true, Some("kobridge")) => "connected",
        (false, false, Some("kobridge")) => "idle",
        _ => "busy",
    };
    Ok(UsbStatus {
        supported: code.is_empty(),
        code: code.into(),
        reason: if code.is_empty() { String::new() } else { probe_reason(code) },
        cable: get("cable") == "1",
        state: state.into(),
        plugin: get("plugin") == "1",
        udc: get("udc").into(),
        udc_state: get("udc_state").into(),
        client: None,
    })
}

/// kindlemtp.koplugin's IPCF4 helper (armhf, built from its `ipcf/kindlemtp-ipcf.c`), for firmware 5.19 on.
const IPCF_BIN: &[u8] = include_bytes!("../resources/kindlemtp-ipcf");
/// Set once the helper is in /tmp; a reboot empties /tmp, so a `no_lipc` answer uploads it again.
static IPCF_UP: AtomicBool = AtomicBool::new(false);

async fn upload_ipcf() {
    if IPCF_UP.swap(true, Relaxed) { return }
    // $$: overlapping first runs (status poll and connect) don't write the same temp file
    let cmd = "p=/tmp/kindlemtp-ipcf.$$ && cat > $p && chmod +x $p && mv $p /tmp/kindlemtp-ipcf";
    if ssh::run_with_input(cmd, IPCF_BIN).await.is_err() { IPCF_UP.store(false, Relaxed) }
}

/// `step` names the command in the timeout error.
async fn run(body: &str, step: &str) -> Res<UsbStatus> {
    upload_ipcf().await;
    let mut s = run_once(body, step).await?;
    if s.code == "no_lipc" && IPCF_UP.swap(false, Relaxed) {
        upload_ipcf().await;
        s = run_once(body, step).await?;
    }
    OURS.store(s.state == "connected", Relaxed);
    Ok(s)
}

async fn run_once(body: &str, step: &str) -> Res<UsbStatus> {
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), ssh::run_script(&script(body))).await
        .map_err(|_| tr(format!("{step}: the Kindle didn't answer within 60 s. Try Reset."), format!("{step}: la Kindle no respondió en 60 s. Probá Reiniciar.")))??;
    parse(&out)
}

/// (label, what `open` gets). macOS: `Settings.mtp_client`, else the first installed app; Linux: gvfs/kio.
fn client(app: &AppHandle) -> Option<(String, String)> {
    if !cfg!(target_os = "macos") {
        return Some((tr("file manager", "administrador de archivos"), "mtp://".into()));
    }
    let choice = super::get_settings(app.clone()).mtp_client;
    let path = if choice.is_empty() {
        let dirs = [std::path::PathBuf::from("/Applications"), super::home().join("Applications")];
        ["KindleMTP", "OpenMTP", "Android File Transfer"].iter()
            .flat_map(|a| dirs.iter().map(move |d| d.join(format!("{a}.app"))))
            .find(|p| p.exists())?.to_string_lossy().into_owned()
    } else { choice }; // an app name; `open -a` finds it wherever it is
    let label = std::path::Path::new(&path).file_stem()?.to_string_lossy().into_owned();
    Some((label, path))
}

fn with_client(app: &AppHandle, mut s: UsbStatus) -> UsbStatus {
    s.client = client(app).map(|c| c.0);
    s
}

#[tauri::command]
pub async fn usb_status(app: AppHandle) -> Res<UsbStatus> {
    Ok(with_client(&app, run(STATUS, "usb_status").await?))
}

/// Connects, then opens the MTP client.
#[tauri::command]
pub async fn usb_connect(app: AppHandle) -> Res<UsbStatus> {
    let s = with_client(&app, run(CONNECT, "usb_connect").await?);
    if s.state == "connected" { let _ = usb_open(app); }
    Ok(s)
}

#[tauri::command]
pub async fn usb_disconnect(app: AppHandle) -> Res<UsbStatus> {
    Ok(with_client(&app, run(DISCONNECT, "usb_disconnect").await?))
}

/// Restarts a wedged responder, then disconnects.
#[tauri::command]
pub async fn usb_reset(app: AppHandle) -> Res<UsbStatus> {
    Ok(with_client(&app, run(RESET, "usb_reset").await?))
}

#[tauri::command]
pub fn usb_open(app: AppHandle) -> Res<()> {
    let (_, path) = client(&app).ok_or(tr("No MTP app is installed.", "No hay ninguna app MTP instalada."))?;
    if cfg!(target_os = "macos") { super::open_with(&["-a", &path]) } else { super::open_with(&[&path]) }
}

/// On quit: tear down only a session KOBridge started, capped at 3 s.
pub fn release_on_exit() {
    if !OURS.swap(false, Relaxed) { return }
    let (tx, rx) = std::sync::mpsc::channel();
    tauri::async_runtime::spawn(async move { let _ = tx.send(ssh::run_script(&script(RELEASE)).await); });
    let _ = rx.recv_timeout(std::time::Duration::from_secs(3));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    const LIST_WITH: &str = include_str!("fixtures/usb/listnames_with_mtp.txt");
    const LIST_WITHOUT: &str = include_str!("fixtures/usb/listnames_without_mtp.txt");
    const PID_MTP: &str = include_str!("fixtures/usb/getpid_tizen_mtp.txt");
    const PID_OTHER: &str = include_str!("fixtures/usb/getpid_other.txt");
    const PID_ERR: &str = include_str!("fixtures/usb/getpid_error_stderr.txt");
    const STAT_T: &str = include_str!("fixtures/usb/proc_stat_T.txt");
    const STAT_S: &str = include_str!("fixtures/usb/proc_stat_S.txt");
    const LOG_HOST: &str = include_str!("fixtures/usb/syslog_state2.txt");
    const LOG_NONE: &str = include_str!("fixtures/usb/syslog_no_state2.txt");
    const TAPE_HOST: &str = include_str!("fixtures/usb/udc_tape_host.txt");
    /// /sys/class/power_supply on the device with the cable unplugged: (name, type, online)
    const POWER_SUPPLY: [(&str, &str, &str); 4] = [
        ("bd71827_bat", "Battery", "1"), ("max20342_moisture", "USB_WET", "1"),
        ("bd71827_ac", "Mains", "0"), ("Wireless", "Wireless", "0"),
    ];
    const TAPE_CHARGER: &str = include_str!("fixtures/usb/udc_tape_charger.txt");

    /// Runs one LIB helper locally (macOS sh + BSD sed here, busybox on the device) with `input` on stdin.
    fn helper(cmd: &str, input: &str) -> String {
        let mut c = Command::new("sh").args(["-c", &format!("{LIB}\n{cmd}")]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        String::from_utf8(c.wait_with_output().unwrap().stdout).unwrap().trim().into()
    }

    #[test]
    fn parses_device_outputs() {
        assert!(helper("names", LIST_WITH).lines().any(|n| n == "com.lab126.mtp"));
        let without = helper("names", LIST_WITHOUT);
        assert!(!without.lines().any(|n| n == "com.lab126.mtp"));
        assert_eq!(helper("names | grep '^:1\\.[0-9][0-9]*$' | head -n 3", LIST_WITHOUT), ":1.0\n:1.1\n:1.2");
        assert_eq!(helper("pid_of", PID_MTP), "5225");
        assert_eq!(helper("pid_of", PID_OTHER), "2214");
        assert_eq!(helper("pid_of", PID_ERR), "");
        assert_eq!(helper("pid_of", "garbage\nuint32 x\n"), "");
        assert_eq!(helper("first", "5225 17\n"), "5225");
        assert_eq!(helper("first", ""), "");
        assert_eq!(helper("pstate", STAT_T), "T");
        assert_eq!(helper("pstate", STAT_S), "S");
        // a comm field holding ") " still yields the state letter after the last one
        assert_eq!(helper("pstate", "9 (a) b) R 1 2\n"), "R");
        assert_eq!(helper("grep -qF \"$STATE2\" && echo y", LOG_HOST), "y");
        assert_eq!(helper("grep -qF \"$STATE2\" && echo y", LOG_NONE), "");
    }

    #[test]
    fn marker_format() {
        assert_eq!(owner("owner=kobridge pid=12 since=1760557063"), Some("kobridge"));
        assert_eq!(owner("owner=plugin pid=1 since=2"), Some("plugin"));
        for m in ["", "owner=kobridge", "owner= pid=1 since=2", "owner=x pid=a since=2", "owner=x pid=1 since=2 extra", "pid=1 owner=x since=2"] {
            assert_eq!(owner(m), None, "{m:?}");
        }
    }

    #[test]
    fn scripts_are_valid_sh() {
        for b in [STATUS, CONNECT, DISCONNECT, RESET, RELEASE] {
            let s = script(b);
            assert!(s.len() < 9000);
            assert!(Command::new("sh").args(["-n", "-c", &s]).status().unwrap().success(), "{b}");
        }
    }

    #[test]
    fn drive_mode_is_always_unicast() {
        let s = script(CONNECT);
        let lines: Vec<&str> = s.lines().filter(|l| l.contains("driveModeStateChanged")).collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("--dest=\"$dest\""));
        // only reached when the bus-name lookup found something
        let guard = s.find("if [ -z \"$dest\" ]; then err=bus_name_not_found").unwrap();
        assert!(guard < s.find("driveModeStateChanged").unwrap());
        for b in [STATUS, DISCONNECT, RESET, RELEASE] { assert!(!script(b).contains("--type=signal")); }
    }

    /// A fake Kindle in a temp dir: the script's paths point into it and the lipc, pidof,
    /// restart, mtp.sh and dbus-send stand-ins log every action to `calls`.
    struct Dev(PathBuf);

    impl Dev {
        fn new(name: &str) -> Dev {
            let t = std::env::temp_dir().join(format!("kob-usb-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&t);
            for d in ["bin", "gadget", "udc/11211000.usb", "proc/2187"] { std::fs::create_dir_all(t.join(d)).unwrap(); }
            let d = Dev(t);
            let exe = |p: &str, body: &str| {
                d.put(p, &format!("#!/bin/sh\n{body}\n"));
                Command::new("chmod").args(["+x"]).arg(d.0.join(p)).status().unwrap();
            };
            exe("bin/lipc-get-prop", r#"echo "$3" >> "$T/gets"
[ -f "$T/lipc_$3" ] && cat "$T/lipc_$3" || exit 1"#);
            exe("bin/lipc-set-prop", r#"echo "lipc $3 $4" >> "$T/calls"
[ -f "$T/refuse_$3" ] && exit 1
case $3 in
  startMtp) echo 1 > "$T/lipc_isMtpStarted"
    [ -f "$T/rotate" ] && : > "$T/messages"
    [ -f "$T/host" ] && cat "$T/host" >> "$T/messages" ;;
  stopMtp) [ -f "$T/sticky" ] || echo 0 > "$T/lipc_isMtpStarted" ;;
  preventScreenSaver) echo "$4" > "$T/lipc_preventScreenSaver" ;;
esac
exit 0"#);
            exe("bin/pidof", r#"r=1; for n; do [ -f "$T/pid_$n" ] && cat "$T/pid_$n" && r=0; done; exit $r"#);
            exe("bin/sync", r#"echo sync >> "$T/calls""#);
            exe("bin/restart", r#"echo "restart $*" >> "$T/calls""#);
            exe("bin/usleep", "sleep 0.05");
            // the Kindle's clock, fixed when `now` exists
            exe("bin/date", r#"if [ "$1" = +%s ] && [ -f "$T/now" ]; then cat "$T/now"; else /bin/date "$@"; fi"#);
            exe("mtp.sh", r#"echo "mtp.sh $*" >> "$T/calls""#);
            // the IPCF4 helper: on 5.18 (no `fw519` file) libipcn is missing; on 5.19 start binds the
            // gadget and the host configures it, stop unbinds it
            exe("ipcf", r#"[ -f "$T/fw519" ] || { echo no_ipcn; exit 2; }
[ "$1" = ping ] || echo "ipcf $1" >> "$T/calls"
[ -f "$T/refuse_ipcf_$1" ] && exit 4
case $1 in
  start) [ -f "$T/late_rebind" ] || echo musb-hdrc.1.auto > "$T/gadget/UDC"; [ -f "$T/host" ] && cat "$T/host" >> "$T/messages" ;;
  stop) [ -f "$T/sticky" ] || : > "$T/gadget/UDC" ;;
esac
echo ok"#);
            exe("dbus-send", r#"for a; do n=$a; done
case "$*" in
  *ListNames*) cat "$T/listnames"; if [ -f "$T/race" ]; then cp "$T/race" "$T/marker"; echo 1 > "$T/lipc_isMtpStarted"; fi ;;
  *GetConnectionUnixProcessID*) n=${n#string:}; if [ -f "$T/getpid_$n" ]; then cat "$T/getpid_$n"; else cat "$T/getpid_err" >&2; exit 1; fi ;;
  *) echo "dbus $*" >> "$T/calls"
     # 5.19 with late_rebind: the responder rebinds the gadget a moment after the signal
     [ -f "$T/late_rebind" ] && ( sleep 0.3; echo musb-hdrc.1.auto > "$T/gadget/UDC" ) & ;;
esac"#);
            // a healthy idle Kindle on a computer's cable, KOReader running (volumd stopped)
            d.put("gadget/UDC", "");
            d.put("udc/11211000.usb/state", "not attached\n");
            d.put("messages", "a\nb\nc\n");
            d.put("pid_tizen-mtp", "5225\n");
            d.put("pid_volumd", "2187\n");
            d.put("proc/2187/stat", STAT_T);
            d.put("lipc_isMtpStarted", "0\n");
            d.put("lipc_isCharging", "1\n");
            d.put("lipc_preventScreenSaver", "0\n");
            d.put("listnames", LIST_WITH);
            d.put("getpid_err", PID_ERR);
            d
        }
        fn put(&self, p: &str, s: &str) { std::fs::write(self.0.join(p), s).unwrap() }
        fn rm(&self, p: &str) { let _ = std::fs::remove_file(self.0.join(p)); }
        fn read(&self, p: &str) -> String { std::fs::read_to_string(self.0.join(p)).unwrap_or_default() }
        fn calls(&self) -> Vec<String> { self.read("calls").lines().map(String::from).collect() }
        fn run(&self, body: &str) -> Res<UsbStatus> {
            let fake = PATHS.lines().map(|l| match l.split_once('=') {
                Some(("WAIT", _)) => "WAIT=1".to_string(),
                Some((k, _)) => format!("{k}=\"$T/{}\"", match k {
                    "MTP_SH" => "mtp.sh", "DBUS" => "dbus-send", "GADGET" => "gadget", "LOG" => "messages",
                    "MARKER" => "marker", "PSS" => "pss", "PLUGIN" => "plugin", "UDCS" => "udc",
                    "POWER" => "power", "BASEUS" => "base-us", "IPCF" => "ipcf", _ => "proc",
                }),
                None => l.into(),
            }).collect::<Vec<_>>().join("\n");
            let s = script(body).replace(PATHS, &format!("{fake}\nPATH=\"$T/bin:$PATH\"\n"));
            // fed on stdin, as ssh::run_script does
            let mut c = Command::new("sh").env("T", &self.0).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
            c.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
            let out = String::from_utf8(c.wait_with_output().unwrap().stdout).unwrap();
            assert!(out.contains("error="), "script stopped early:\n{out}");
            parse(&out)
        }
    }

    impl Drop for Dev {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn status_probe_failures() {
        let d = Dev::new("probe");
        let s = d.run(STATUS).unwrap();
        assert!(s.supported && s.cable && !s.plugin, "{s:?}");
        // lipc reads can block for 10 s each: a status reads each property once
        assert_eq!(d.read("gets"), "isMtpStarted\nisCharging\n");
        d.run(RELEASE).unwrap();
        assert_eq!(d.read("gets").lines().count(), 4);
        assert_eq!((s.state.as_str(), s.code.as_str()), ("idle", ""));
        // each check fails on its own, in the plugin's order
        type Break = fn(&Dev);
        let cases: [(&str, Break); 6] = [
            ("no_mtp_sh", |d| d.rm("mtp.sh")),
            ("no_gadget", |d| { std::fs::remove_dir_all(d.0.join("gadget")).unwrap(); }),
            ("no_responder", |d| d.rm("pid_tizen-mtp")),
            ("no_dbus_send", |d| d.rm("dbus-send")),
            ("no_lipc", |d| d.rm("lipc_isMtpStarted")),
            ("volumd_running", |d| d.put("proc/2187/stat", STAT_S)),
        ];
        for (code, break_it) in cases {
            let d = Dev::new(code);
            break_it(&d);
            let s = d.run(STATUS).unwrap();
            assert!(!s.supported && s.code == code && !s.reason.is_empty(), "{code}: {s:?}");
            // connect refuses without touching the device
            assert!(d.run(CONNECT).is_ok_and(|s| s.code == code));
            assert!(d.calls().is_empty(), "{code}: {:?}", d.calls());
        }
        // volumd not running at all is fine (the plugin's rule: no pid, not running)
        let d = Dev::new("novolumd");
        d.rm("pid_volumd");
        assert!(d.run(STATUS).unwrap().supported);
    }

    #[test]
    fn status_power_and_marker() {
        let d = Dev::new("marker");
        d.put("lipc_isCharging", "0\n");
        assert!(!d.run(STATUS).unwrap().cable);
        d.rm("lipc_isCharging");
        assert!(!d.run(STATUS).unwrap().cable);
        d.put("lipc_isCharging", "1\n");
        std::fs::create_dir_all(d.0.join("plugin")).unwrap();
        assert!(d.run(STATUS).unwrap().plugin);

        d.put("lipc_isMtpStarted", "1\n");
        d.put("marker", "owner=kobridge pid=9 since=1760557063\n");
        assert_eq!(d.run(STATUS).unwrap().state, "connected");
        d.put("marker", "owner=plugin pid=9 since=1760557063\n");
        assert_eq!(d.run(STATUS).unwrap().state, "busy");
        d.put("marker", "junk\n");
        assert_eq!(d.run(STATUS).unwrap().state, "busy");
        // isMtpStarted 0: stale, removed by the reader
        d.put("lipc_isMtpStarted", "0\n");
        d.put("marker", "owner=plugin pid=9 since=1760557063\n");
        assert_eq!(d.run(STATUS).unwrap().state, "idle");
        assert!(!d.0.join("marker").exists());
        // the 30 s boundary by the Kindle's clock; unparsable and future since count as fresh
        d.put("now", "1760557100\n");
        for (since, kept) in [("1760557071", true), ("1760557070", false), ("1760557200", true)] {
            d.put("marker", &format!("owner=plugin pid=9 since={since}\n"));
            let s = d.run(STATUS).unwrap();
            assert_eq!(d.0.join("marker").exists(), kept, "since={since}");
            assert_eq!(s.state, if kept { "busy" } else { "idle" }, "since={since}");
        }
        for m in ["junk\n", "owner=plugin pid=9 since=\n", "owner=plugin pid=9 since=12x\n"] {
            d.put("marker", m);
            d.run(STATUS).unwrap();
            assert!(d.0.join("marker").exists(), "{m:?}");
        }
        // connect refuses a fresh claim even before its startMtp
        d.put("marker", "owner=plugin pid=9 since=1760557090\n");
        assert_eq!(d.run(CONNECT).unwrap_err(), step_error("owner_plugin"));
        assert!(d.calls().is_empty());
        d.rm("now");
        // a failed lipc read keeps it
        d.rm("lipc_isMtpStarted");
        d.put("marker", "owner=plugin pid=9 since=1760557063\n");
        assert_eq!(d.run(STATUS).unwrap().code, "no_lipc");
        assert!(d.0.join("marker").exists());
    }

    /// Commands in `calls` matching the recipe steps, in order.
    fn steps(d: &Dev) -> Vec<String> { d.calls().into_iter().map(|c| c.split(" /default").next().unwrap().into()).collect() }

    #[test]
    fn connect_follows_the_recipe() {
        let d = Dev::new("connect");
        d.put("host", LOG_HOST);
        let s = d.run(CONNECT).unwrap();
        assert_eq!(s.state, "connected");
        assert_eq!(steps(&d), ["lipc preventScreenSaver 1", "mtp.sh udc disable", "lipc stopMtp 1", "lipc startMtp 1", "dbus --system --dest=com.lab126.mtp --type=signal"]);
        assert!(d.calls()[4].ends_with("com.lab126.volumd.driveModeStateChanged int32:1"));
        let m = d.read("marker");
        assert!(owner(m.trim()) == Some("kobridge"), "{m}");
        // a second connect while ours is valid does nothing
        std::fs::remove_file(d.0.join("calls")).unwrap();
        assert_eq!(d.run(CONNECT).unwrap().state, "connected");
        assert!(d.calls().is_empty());
        // the host is detected only from lines written after startMtp
        let d = Dev::new("oldlog");
        d.put("messages", LOG_HOST);
        assert!(d.run(CONNECT).unwrap_err().contains("20 s"));
        // syslog rotated after startMtp: the new, shorter file is read whole
        let d = Dev::new("rotate");
        d.put("messages", &"x\n".repeat(100));
        d.put("rotate", "");
        d.put("host", LOG_HOST);
        assert_eq!(d.run(CONNECT).unwrap().state, "connected");
    }

    #[test]
    fn connect_finds_the_bus_name_by_pid() {
        let d = Dev::new("bus");
        d.put("listnames", LIST_WITHOUT);
        d.put("getpid_:1.2", PID_OTHER);
        d.put("getpid_:1.3", PID_MTP);
        d.put("host", LOG_HOST);
        assert_eq!(d.run(CONNECT).unwrap().state, "connected");
        assert!(d.calls().last().unwrap().starts_with("dbus --system --dest=:1.3 --type=signal"), "{:?}", d.calls());
        // no match: refused before touching the gadget, never a broadcast
        let d = Dev::new("nobus");
        d.put("listnames", LIST_WITHOUT);
        d.put("host", LOG_HOST);
        assert_eq!(d.run(CONNECT).unwrap_err(), step_error("bus_name_not_found"));
        assert!(d.calls().is_empty());
    }

    #[test]
    fn connect_refusals_and_failures() {
        // the charger tape: the UDC stays bound and no state-2 line ever comes
        let last = |tape: &str| tape.lines().next_back().unwrap().split(' ').find_map(|f| f.strip_prefix("udc=[")).unwrap().trim_end_matches(']').to_string();
        let d = Dev::new("charger");
        d.put("gadget/UDC", &last(TAPE_CHARGER));
        assert_eq!(d.run(CONNECT).unwrap_err(), step_error("no_host"));
        assert_eq!(steps(&d), ["lipc preventScreenSaver 1", "mtp.sh udc disable", "lipc stopMtp 1", "lipc startMtp 1",
                               "lipc stopMtp 1", "mtp.sh udc disable", "sync", "lipc preventScreenSaver 0"]);
        assert!(!d.0.join("marker").exists() && !d.0.join("pss").exists());
        assert_eq!(last(TAPE_HOST), "11211000.usb");

        let d = Dev::new("nopower");
        d.put("lipc_isCharging", "0\n");
        assert_eq!(d.run(CONNECT).unwrap_err(), step_error("no_power"));
        assert!(d.calls().is_empty());

        let d = Dev::new("plugin");
        d.put("lipc_isMtpStarted", "1\n");
        d.put("marker", "owner=plugin pid=9 since=1760557063\n");
        assert_eq!(d.run(CONNECT).unwrap_err(), step_error("owner_plugin"));
        assert!(d.calls().is_empty());
        d.put("marker", "junk\n");
        assert_eq!(d.run(CONNECT).unwrap_err(), step_error("owner_other"));

        let d = Dev::new("refuse");
        d.put("refuse_startMtp", "");
        assert_eq!(d.run(CONNECT).unwrap_err(), step_error("startmtp_failed"));
        assert_eq!(steps(&d)[4..], ["lipc stopMtp 1", "mtp.sh udc disable", "sync", "lipc preventScreenSaver 0"]);
        assert!(!d.0.join("marker").exists());

        // lost the set -C claim to the plugin: no lipc or mtp.sh command, its session stays up
        let d = Dev::new("race");
        d.put("race", "owner=plugin pid=9 since=1760557063\n");
        assert_eq!(d.run(CONNECT).unwrap_err(), step_error("marker_failed"));
        assert!(d.calls().is_empty(), "{:?}", d.calls());
        assert_eq!(d.read("marker"), "owner=plugin pid=9 since=1760557063\n");
    }

    #[test]
    fn cable_counts_a_full_battery() {
        // the Reddit bug: battery Full reads isCharging 0 with the cable still in
        let d = Dev::new("cable");
        d.put("lipc_isCharging", "0\n");
        assert!(!d.run(STATUS).unwrap().cable);
        d.put("udc/11211000.usb/state", "powered\n");
        assert!(d.run(STATUS).unwrap().cable);
        d.put("udc/11211000.usb/state", "not attached\n");
        // the real unplugged snapshot (Paperwhite, battery Discharging): the battery and the
        // moisture sensor read online=1 and must not count
        for (name, kind, online) in POWER_SUPPLY {
            std::fs::create_dir_all(d.0.join(format!("power/{name}"))).unwrap();
            d.put(&format!("power/{name}/type"), &format!("{kind}\n"));
            d.put(&format!("power/{name}/online"), &format!("{online}\n"));
        }
        assert!(!d.run(STATUS).unwrap().cable);
        let s = d.run(CONNECT).unwrap_err();
        assert_eq!(s, step_error("no_power"));
        d.put("power/bd71827_ac/online", "1\n");
        assert!(d.run(STATUS).unwrap().cable);
        for kind in ["USB", "USB_DCP", "USB_C", "USB_PD"] {
            d.put("power/bd71827_ac/type", &format!("{kind}\n"));
            assert!(d.run(STATUS).unwrap().cable, "{kind}");
        }
        d.put("power/bd71827_ac/type", "Wireless\n");
        assert!(!d.run(STATUS).unwrap().cable);
        d.put("power/bd71827_ac/type", "Mains\n");
        d.put("host", LOG_HOST);
        assert_eq!(d.run(CONNECT).unwrap().state, "connected");
    }

    #[test]
    fn refuses_with_a_book_open() {
        let open = |name: &str, pid: &str, targets: &[&str]| {
            let d = Dev::new(name);
            d.put(&format!("pid_{}", if pid == "1" { "luajit" } else { "reader.lua" }), &format!("{pid}\n"));
            let fd = d.0.join(format!("proc/{pid}/fd"));
            std::fs::create_dir_all(&fd).unwrap();
            for (i, t) in targets.iter().enumerate() { std::os::unix::fs::symlink(t, fd.join(i.to_string())).unwrap(); }
            d.put("host", LOG_HOST);
            let r = d.run(CONNECT);
            (d, r)
        };
        for (pid, t) in [("1", "/mnt/us/books/A b (1).epub"), ("2", "/mnt/us/documents/x.pdf"), ("1", "/mnt/us/koreader/cache/cr3cache/A.cr3")] {
            let (d, r) = open(&format!("book{pid}{}", t.len()), pid, &["/dev/null", "/mnt/us/koreader/settings.reader.lua", t]);
            assert_eq!(r.unwrap_err(), step_error("book_open"), "{t}");
            assert!(d.calls().is_empty());
        }
        // KOReader's own files, the file browser and other folders are fine
        let (_, r) = open("nobook", "1", &["/mnt/us/koreader/settings/statistics.sqlite3", "/mnt/us/koreader/fonts/x.ttf", "/tmp/x", "socket:[123]"]);
        assert_eq!(r.unwrap().state, "connected");
    }

    #[test]
    fn cleans_mtp_tmp_only_when_mtp_is_free() {
        let d = Dev::new("tmp");
        std::fs::create_dir_all(d.0.join("base-us/.mtp-tmp/part")).unwrap();
        d.put("lipc_isMtpStarted", "1\n");
        d.put("sticky", ""); // stopMtp didn't take: isMtpStarted still reads 1
        d.run(DISCONNECT).unwrap();
        assert!(d.0.join("base-us/.mtp-tmp").exists());
        d.rm("sticky");
        d.run(DISCONNECT).unwrap();
        assert!(!d.0.join("base-us/.mtp-tmp").exists());
        assert!(d.0.join("base-us").exists());
    }

    #[test]
    fn keeps_the_kindle_awake_and_restores() {
        let d = Dev::new("pss");
        d.put("lipc_preventScreenSaver", "1\n"); // a KeepAlive user
        d.put("host", LOG_HOST);
        d.run(CONNECT).unwrap();
        assert_eq!(d.read("pss").trim(), "1");
        d.put("lipc_preventScreenSaver", "0\n");
        d.run(CONNECT).unwrap(); // already ours: the saved value stays
        assert_eq!(d.read("pss").trim(), "1");
        d.run(DISCONNECT).unwrap();
        assert_eq!(d.read("lipc_preventScreenSaver").trim(), "1");
        assert!(!d.0.join("pss").exists());
        // an unreadable value restores 0
        let d = Dev::new("pss0");
        d.rm("lipc_preventScreenSaver");
        d.put("host", LOG_HOST);
        d.run(CONNECT).unwrap();
        assert_eq!(d.read("pss").trim(), "0");
        d.run(DISCONNECT).unwrap();
        assert_eq!(d.calls().last().unwrap(), "lipc preventScreenSaver 0");
    }

    /// Firmware 5.19: no lipc properties for com.lab126.mtp, the helper does start and stop.
    #[test]
    fn firmware_519_uses_the_ipcf_helper() {
        let d = Dev::new("fw519");
        d.put("fw519", "");
        d.rm("lipc_isMtpStarted");
        let s = d.run(STATUS).unwrap();
        assert_eq!((s.code.as_str(), s.state.as_str()), ("", "idle"));
        d.put("host", LOG_HOST);
        let s = d.run(CONNECT).unwrap();
        assert_eq!(s.state, "connected");
        assert_eq!(steps(&d), ["lipc preventScreenSaver 1", "mtp.sh udc disable", "ipcf stop", "ipcf start", "dbus --system --dest=com.lab126.mtp --type=signal"]);
        assert!(!d.read("gets").contains("isMtpStarted"));
        // stop goes through the helper; the unbound UDC reads as stopped, so .mtp-tmp is cleaned
        std::fs::create_dir_all(d.0.join("base-us/.mtp-tmp")).unwrap();
        d.put("calls", "");
        let s = d.run(DISCONNECT).unwrap();
        assert_eq!(s.state, "idle");
        assert_eq!(steps(&d), ["ipcf stop", "mtp.sh udc disable", "sync", "lipc preventScreenSaver 0"]);
        assert!(!d.0.join("base-us/.mtp-tmp").exists());
        // a refused start tears down through the helper
        d.put("calls", "");
        d.put("refuse_ipcf_start", "");
        assert!(d.run(CONNECT).is_err());
        assert_eq!(steps(&d)[3..], ["ipcf start", "ipcf stop", "mtp.sh udc disable", "sync", "lipc preventScreenSaver 0"]);
        // the UDC comes back only after the signal (state 2 -> 3): connect waits for it
        d.rm("refuse_ipcf_start");
        d.run(DISCONNECT).unwrap();
        d.put("late_rebind", "");
        assert_eq!(d.run(CONNECT).unwrap().state, "connected");
        // the plugin's copy wins when installed
        std::fs::create_dir_all(d.0.join("plugin/bin")).unwrap();
        std::fs::copy(d.0.join("ipcf"), d.0.join("plugin/bin/kindlemtp-ipcf")).unwrap();
        d.rm("ipcf");
        assert_eq!(d.run(STATUS).unwrap().code, "");
        d.rm("plugin/bin/kindlemtp-ipcf");
        // no helper and no lipc: unsupported
        d.rm("fw519");
        assert_eq!(d.run(STATUS).unwrap().code, "no_lipc");
    }

    #[test]
    fn disconnect_reset_and_release() {
        let d = Dev::new("stop");
        d.put("lipc_isMtpStarted", "1\n");
        d.put("marker", "owner=plugin pid=9 since=1760557063\n");
        // stop tears down whoever owns the marker, stopMtp before udc disable
        assert_eq!(d.run(DISCONNECT).unwrap().state, "idle");
        assert_eq!(steps(&d), ["lipc stopMtp 1", "mtp.sh udc disable", "sync"]);
        assert!(!d.0.join("marker").exists());

        let d = Dev::new("reset");
        assert_eq!(d.run(RESET).unwrap().state, "idle");
        assert_eq!(steps(&d), ["restart mtp", "lipc stopMtp 1", "mtp.sh udc disable", "sync"]);

        // quitting releases only KOBridge's own session
        let d = Dev::new("release");
        d.put("lipc_isMtpStarted", "1\n");
        d.put("marker", "owner=plugin pid=9 since=1760557063\n");
        d.run(RELEASE).unwrap();
        assert!(d.calls().is_empty());
        d.put("marker", "owner=kobridge pid=9 since=1760557063\n");
        d.run(RELEASE).unwrap();
        assert_eq!(steps(&d), ["lipc stopMtp 1", "mtp.sh udc disable", "sync"]);
    }
}

/// Against the real Kindle on a computer's cable: `cargo test kindle_usb -- --ignored --nocapture`
#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore]
    fn kindle_usb() {
        tauri::async_runtime::block_on(async {
            let s = run(STATUS, "usb_status").await.expect("status");
            println!("{s:?}");
            assert!(s.supported && s.cable, "{s:?}");
            let s = run(CONNECT, "usb_connect").await.expect("connect");
            assert_eq!(s.state, "connected");
            assert_eq!(ssh::run("echo ok").await.expect("ssh after drive mode").trim(), "ok");
            let s = run(DISCONNECT, "usb_disconnect").await.expect("disconnect");
            assert_eq!((s.state.as_str(), s.udc.as_str()), ("idle", ""));
        });
    }
}
