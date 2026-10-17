//! Everything goes through the system ssh, so ~/.ssh/config decides the IP, the key and
//! the known_hosts entry. `ssh kindle` evaluates every `Match exec` probe in that config
//! on each call (~2 s with two dead IPs), so `resolve()` runs `ssh -G kindle` once and
//! later calls target the resolved IP with explicit options (~50 ms on a warm mux).

use std::path::Path;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

pub const HOST: &str = "kindle";

pub type Res<T> = Result<T, String>;

static SPANISH: AtomicBool = AtomicBool::new(false);

/// Set from `Settings.lang`; every message shown to the user goes through `tr`.
pub fn set_spanish(on: bool) { SPANISH.store(on, Relaxed) }

pub fn tr(en: impl Into<String>, es: impl Into<String>) -> String {
    if SPANISH.load(Relaxed) { es.into() } else { en.into() }
}

pub fn unreachable() -> String {
    tr("The Kindle doesn't answer on any of its known IPs.", "La Kindle no responde en ninguna de sus IPs conocidas.")
}

/// Explicit ssh args ending with the destination IP, or None to use the `kindle` alias.
static TARGET: Mutex<Option<Vec<String>>> = Mutex::new(None);

pub fn target() -> Option<Vec<String>> { TARGET.lock().unwrap().clone() }
pub fn set_target(t: Option<Vec<String>>) { *TARGET.lock().unwrap() = t; }

/// Host shown in the UI: the cached IP, or the alias when unresolved.
pub fn host() -> String {
    target().and_then(|t| t.last().cloned()).unwrap_or_else(|| HOST.into())
}

/// Connection-level failures mean the cached IP may be stale (network switch, DHCP).
pub fn is_connect_error(msg: &str) -> bool {
    ["timed out", "No route to host", "Connection refused", "Network is unreachable",
     "Host key verification failed", "Could not resolve", "Host is down"]
        .iter().any(|p| msg.contains(p))
}

/// Ask ssh which host/port/user/key `kindle` maps to right now (runs the Match probes once).
pub async fn resolve() -> Res<Option<Vec<String>>> {
    let out = Command::new("/usr/bin/ssh").args(["-G", HOST]).output().await.map_err(|e| e.to_string())?;
    parse_g(&String::from_utf8_lossy(&out.stdout))
}

fn parse_g(text: &str) -> Res<Option<Vec<String>>> {
    let mut args: Vec<String> = Vec::new();
    let mut host = String::new();
    let mut port = String::from("22");
    let mut alias = false;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(' ') else { continue };
        match k {
            "hostname" => host = v.to_string(),
            "port" => { port = v.to_string(); args.extend(["-p".into(), v.into()]) }
            "user" => args.extend(["-l".into(), v.into()]),
            "identityfile" => args.extend(["-i".into(), v.into()]),
            "identitiesonly" | "userknownhostsfile" | "addkeystoagent" => args.extend(["-o".into(), format!("{k}={v}")]),
            "hostkeyalias" => { alias = true; args.extend(["-o".into(), format!("HostKeyAlias={v}")]) }
            // a proxied host can't be reached by IP; keep the alias
            "proxycommand" | "proxyjump" if v != "none" => return Ok(None),
            _ => {}
        }
    }
    if host.is_empty() || host == HOST {
        return Err(unreachable()); // every Match probe failed
    }
    if !alias {
        // keep the known_hosts entry the alias would have used: ssh names it
        // `[host]:port` off port 22, and HostKeyAlias is looked up literally
        let key = if port == "22" { host.clone() } else { format!("[{host}]:{port}") };
        args.extend(["-o".into(), format!("HostKeyAlias={key}")]);
    }
    // UseKeychain only exists in Apple's ssh; Linux ssh rejects the whole command
    if cfg!(target_os = "macos") { args.extend(["-o".into(), "UseKeychain=yes".into()]); }
    args.push(host);
    Ok(Some(args))
}

fn ssh() -> Command {
    let mut c = Command::new("/usr/bin/ssh");
    c.args([
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=4",
        "-o", "ServerAliveInterval=10",
        "-o", "ControlMaster=auto",
        "-o", "ControlPath=~/.ssh/kob-%C",
        "-o", "ControlPersist=600",
    ]);
    match target() {
        Some(t) => { c.args(t); }
        None => { c.arg(HOST); }
    }
    c.stdin(Stdio::null()).kill_on_drop(true);
    c
}

/// Single-quote for the remote POSIX shell.
pub fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn fail(stderr: &[u8], code: Option<i32>) -> String {
    let msg = String::from_utf8_lossy(stderr).trim().to_string();
    if is_connect_error(&msg) { set_target(None); }
    if msg.is_empty() { tr(format!("ssh exited with code {}", code.unwrap_or(-1)), format!("ssh terminó con código {}", code.unwrap_or(-1))) } else { msg }
}

pub async fn run(cmd: &str) -> Res<String> {
    let out = ssh().arg(cmd).output().await.map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(fail(&out.stderr, out.status.code()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run with `input` on stdin (used to write remote files without quoting their content).
pub async fn run_with_input(cmd: &str, input: &[u8]) -> Res<()> {
    let mut child = ssh().arg(cmd).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped())
        .spawn().map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).await.map_err(|e| e.to_string())?;
    drop(stdin);
    let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(fail(&out.stderr, out.status.code()));
    }
    Ok(())
}

/// Run a script fed to the remote `sh` on stdin, for scripts that grow with a selection:
/// dropbear refuses exec requests longer than 9000 bytes.
pub async fn run_script(script: &str) -> Res<String> {
    let mut child = ssh().arg("sh").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(script.as_bytes()).await.map_err(|e| e.to_string())?;
    drop(stdin);
    let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(fail(&out.stderr, out.status.code()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Stream remote stdout into a local file, reporting bytes written.
pub async fn pull_to_file(cmd: &str, dest: &Path, mut progress: impl FnMut(u64)) -> Res<u64> {
    let mut child = ssh().arg(cmd).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| e.to_string())?;
    let mut src = child.stdout.take().unwrap();
    let tmp = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&tmp).await.map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 256 * 1024];
    let (mut total, mut last) = (0u64, 0u64);
    loop {
        let chunk = match src.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => file.write_all(&buf[..n]).await.map(|_| n),
            Err(e) => Err(e),
        };
        let n = match chunk {
            Ok(n) => n,
            Err(e) => { let _ = tokio::fs::remove_file(&tmp).await; return Err(e.to_string()); }
        };
        total += n as u64;
        if total - last >= 1 << 20 { progress(total); last = total; }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if !out.status.success() {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(fail(&out.stderr, out.status.code()));
    }
    tokio::fs::rename(&tmp, dest).await.map_err(|e| e.to_string())?;
    progress(total);
    Ok(total)
}

/// Copy one local file into a remote directory. `cat > file` instead of scp because
/// scp's remote side word-splits names with spaces and parentheses.
pub async fn upload(local: &Path, remote_dir: &str) -> Res<String> {
    let name = local.file_name().and_then(|n| n.to_str()).ok_or(tr("invalid file name", "nombre de archivo inválido"))?.to_string();
    let target = format!("{}/{}", remote_dir.trim_end_matches('/'), name);
    let file = std::fs::File::open(local).map_err(|e| e.to_string())?;
    let cmd = format!("mkdir -p {d} && cat > {t}.kob-part && mv {t}.kob-part {t}", d = sq(remote_dir), t = sq(&target));
    let out = ssh().arg(cmd).stdin(Stdio::from(file)).stdout(Stdio::null()).stderr(Stdio::piped())
        .output().await.map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(fail(&out.stderr, out.status.code()));
    }
    Ok(target)
}

/// Copy one local folder into a remote directory: local tar piped into remote tar.
/// COPYFILE_DISABLE keeps macOS from adding `._` files.
pub async fn upload_dir(local: &Path, remote_dir: &str) -> Res<String> {
    let name = local.file_name().and_then(|n| n.to_str()).ok_or(tr("invalid folder name", "nombre de carpeta inválido"))?;
    let parent = local.parent().ok_or(tr("invalid folder name", "nombre de carpeta inválido"))?;
    let mut tar = Command::new("tar").env("COPYFILE_DISABLE", "1")
        .args(["-cf", "-", "--exclude", ".DS_Store", "-C"]).arg(parent).arg(name)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true)
        .spawn().map_err(|e| e.to_string())?;
    let pipe: Stdio = tar.stdout.take().unwrap().try_into().map_err(|e: std::io::Error| e.to_string())?;
    let cmd = format!("mkdir -p {d} && tar -xof - -C {d}", d = sq(remote_dir));
    let out = ssh().arg(cmd).stdin(pipe).stdout(Stdio::null()).stderr(Stdio::piped())
        .output().await.map_err(|e| e.to_string())?;
    let local_out = tar.wait_with_output().await.map_err(|e| e.to_string())?;
    // ssh first: when it fails, tar's own error is only the broken pipe
    if !out.status.success() {
        return Err(fail(&out.stderr, out.status.code()));
    }
    if !local_out.status.success() {
        return Err(String::from_utf8_lossy(&local_out.stderr).trim().to_string());
    }
    Ok(format!("{}/{}", remote_dir.trim_end_matches('/'), name))
}

/// Spawn a long-running remote command; caller reads its stdout.
pub fn stream(cmd: &str) -> Res<Child> {
    ssh().arg(cmd).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_explicit_target() {
        let g = "user root\nhostname 192.168.0.38\nport 2222\nidentitiesonly yes\nhostkeyalias kindle\n\
                 identityfile ~/.ssh/id_ed25519_kindle\nuserknownhostsfile /a /b\nproxycommand none\n";
        let t = parse_g(g).unwrap().unwrap();
        assert_eq!(t.last().unwrap(), "192.168.0.38");
        for a in ["2222", "root", "~/.ssh/id_ed25519_kindle", "HostKeyAlias=kindle", "userknownhostsfile=/a /b"] {
            assert!(t.iter().any(|x| x == a), "missing {a}");
        }
        assert_eq!(t.iter().any(|x| x == "UseKeychain=yes"), cfg!(target_os = "macos"));
        // no alias in config: pin known_hosts to the IP the alias would have used
        let t = parse_g("hostname 10.0.0.5\nport 22\n").unwrap().unwrap();
        assert!(t.iter().any(|x| x == "HostKeyAlias=10.0.0.5"));
        // off port 22 ssh saved the key as [ip]:port (the block Settings writes uses 2222)
        let t = parse_g("hostname 192.168.0.111\nport 2222\n").unwrap().unwrap();
        assert!(t.iter().any(|x| x == "HostKeyAlias=[192.168.0.111]:2222"));
        // every Match probe failed
        assert_eq!(parse_g("hostname kindle\nport 2222\n").unwrap_err(), unreachable());
        // proxied hosts keep the alias
        assert_eq!(parse_g("hostname 10.0.0.5\nproxyjump bastion\n").unwrap(), None);
    }
    #[test]
    fn quotes_single_quotes() {
        assert_eq!(sq("it's"), r"'it'\''s'");
        assert_eq!(sq("a b (1).epub"), "'a b (1).epub'");
    }
}
