//! The Files view: browse and change anything under /mnt/us. Its own guard instead of
//! `allowed()`: system folders and most of `koreader/` stay read-only.
//! Batch commands print `OK|EXISTS|ERR|KEPT <path>` per item, then the new listing of each
//! affected folder after a `@@KOB@@ <dir>` line, so a change costs one ssh round trip.

use crate::ssh::{self, sq, tr, Res};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tauri::{AppHandle, Manager};

pub const US: &str = "/mnt/us";
const MARK: &str = "@@KOB@@ ";
const NODIR: &str = "@@KOB-NODIR@@";
/// Never renamed, moved or deleted.
const PROTECTED: [&str; 9] = ["documents", "audible", "kpm", "voice", "libkh", "mrpackages", "system", "extensions", "koreader"];
/// The only writable subtrees inside koreader/ (plus settings/ while KOReader is closed).
const KO_RW: [&str; 6] = ["patches", "plugins", "fonts", "styletweaks", "screenshots", "data"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry { pub name: String, pub kind: String, pub size: u64, pub mtime: u64 }

#[derive(Serialize, Debug, PartialEq)]
pub struct Outcome { pub state: String, pub path: String }

#[derive(Serialize, Default)]
pub struct Change { pub results: Vec<Outcome>, pub dirs: HashMap<String, Vec<Entry>> }

/// `/mnt/us` or below it, with no `.`, `..` or empty segment and no newline.
pub fn under_us(p: &str) -> bool {
    if p.contains(['\n', '\0']) { return false }
    p == US || p.strip_prefix("/mnt/us/").is_some_and(|r| r.split('/').all(|s| !s.is_empty() && s != "." && s != ".."))
}

fn not_allowed(p: &str) -> String { tr(format!("path not allowed: {p}"), format!("ruta no permitida: {p}")) }

/// Whether `p` may be renamed, moved, deleted or created; `Ok(true)` when KOReader must be closed.
fn check(p: &str) -> Res<bool> {
    if !under_us(p) { return Err(not_allowed(p)) }
    let segs: Vec<&str> = p[US.len()..].split('/').skip(1).collect();
    match segs[..] {
        [] => Err(tr("/mnt/us can't be changed.", "/mnt/us no se puede cambiar.")),
        [top] if PROTECTED.contains(&top) => Err(tr(format!("{p} is a system folder and can't be renamed, moved or deleted."),
                                                    format!("{p} es una carpeta del sistema y no se puede renombrar, mover ni borrar."))),
        ["koreader", sub, _, ..] if KO_RW.contains(&sub) => Ok(false),
        ["koreader", "settings", _, ..] => Ok(true),
        ["koreader", ..] => Err(tr(format!("{p} is read-only. Inside koreader/ only patches, plugins, fonts, styletweaks, screenshots and data can change."),
                                   format!("{p} es de solo lectura. Dentro de koreader/ solo cambian patches, plugins, fonts, styletweaks, screenshots y data."))),
        _ => Ok(false),
    }
}

async fn guard(paths: &[&str]) -> Res<()> {
    let mut settings = false;
    for p in paths { settings |= check(p)?; }
    if settings && super::reader_running().await? {
        return Err(tr("KOReader is open and would overwrite its settings when it exits. Close it on the Kindle and try again.",
                      "KOReader está abierto y sobrescribiría su configuración al cerrar. Cerralo en la Kindle y volvé a intentar."));
    }
    Ok(())
}

fn parent(p: &str) -> &str { p.rsplit_once('/').map_or("", |x| x.0) }
fn base(p: &str) -> &str { p.rsplit_once('/').map_or(p, |x| x.1) }

/// KOReader's sidecar folder for a book and the book's extension: `X.sdr` for `X.epub`
/// (the name without its LAST extension). The shell still checks that the book is a regular file.
fn sidecar(p: &str) -> Option<(String, &str)> {
    let (dir, name) = p.rsplit_once('/')?;
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && ext != "sdr").then(|| (format!("{dir}/{stem}.sdr"), ext))
}

/// A selected `X.sdr` whose book is also selected is handled once, as that book's sidecar.
fn dedupe(paths: &[String]) -> Vec<String> {
    let books: HashSet<String> = paths.iter().filter_map(|q| sidecar(q).map(|x| x.0)).collect();
    paths.iter().filter(|p| !books.contains(*p)).cloned().collect()
}

fn list_cmd(dir: &str, depth: &str) -> String {
    format!("find {} -mindepth 1 {depth} -name '.?*' -prune -o -exec stat -c '%F|%s|%Y|%n' {{}} + 2>/dev/null; true", sq(dir))
}

/// `stat` lines into (path, entry). A line that isn't `type|size|mtime|path` continues the
/// previous name (a name with a newline), which `under_us` then refuses for any action.
pub fn parse_stat(out: &str) -> Vec<(String, Entry)> {
    let mut v: Vec<(String, Entry)> = vec![];
    for line in out.lines() {
        let f: Vec<&str> = line.splitn(4, '|').collect();
        if let [kind, size, mtime, path] = f[..] && let (Ok(size), Ok(mtime)) = (size.parse(), mtime.parse()) {
            let kind = if kind.starts_with("directory") { "dir" } else if kind.starts_with("symbolic link") { "link" } else { "file" };
            v.push((path.into(), Entry { name: String::new(), kind: kind.into(), size, mtime }));
        } else if let Some((p, _)) = v.last_mut() {
            p.push('\n');
            p.push_str(line);
        }
    }
    for (p, e) in &mut v { e.name = base(p).into(); }
    v
}

/// Entries grouped by folder; every directory gets a key, even an empty one.
fn group(v: Vec<(String, Entry)>) -> HashMap<String, Vec<Entry>> {
    let mut m: HashMap<String, Vec<Entry>> = HashMap::new();
    for (p, e) in v {
        if e.kind == "dir" { m.entry(p.clone()).or_default(); }
        m.entry(parent(&p).into()).or_default().push(e);
    }
    m
}

fn parse_change(out: &str) -> Change {
    let mut c = Change::default();
    let mut cur: Option<(String, String)> = None;
    let flush = |c: &mut Change, cur: Option<(String, String)>| if let Some((d, buf)) = cur {
        c.dirs.insert(d, parse_stat(&buf).into_iter().map(|x| x.1).collect());
    };
    for line in out.lines() {
        if let Some(d) = line.strip_prefix(MARK) {
            flush(&mut c, cur.take());
            cur = Some((d.into(), String::new()));
        } else if let Some((_, buf)) = &mut cur {
            buf.push_str(line);
            buf.push('\n');
        } else if let Some((state, path)) = line.split_once(' ') {
            c.results.push(Outcome { state: state.into(), path: path.into() });
        }
    }
    flush(&mut c, cur);
    c
}

/// Rename or move one item, never over an existing one; the book's `X.sdr` follows it
/// unless it also holds another format's `metadata.<ext>.lua`.
fn mv_item(s: &str, t: &str) -> String {
    let (qs, qt) = (sq(s), sq(t));
    let carry = match (sidecar(s), sidecar(t)) {
        (Some((ss, ext)), Some((ts, _))) if ss != ts => {
            let (qss, qts, own) = (sq(&ss), sq(&ts), sq(&format!("{ss}/metadata.{ext}.lua")));
            format!("if [ -f {qt} ] && [ -d {qss} ]; then o=0; for m in {qss}/metadata.*.lua; do [ -e \"$m\" ] && [ \"$m\" != {own} ] && o=1; done; \
                     if [ $o = 0 ] && [ ! -e {qts} ] && mv -nT {qss} {qts}; then :; else printf 'KEPT %s\\n' {qss}; fi; fi; ")
        }
        _ => String::new(),
    };
    format!("if [ -e {qt} ]; then printf 'EXISTS %s\\n' {qs}; elif mv -nT {qs} {qt}; then printf 'OK %s\\n' {qs}; {carry}else printf 'ERR %s\\n' {qs}; fi")
}

/// Delete one item; a book's sidecar loses only that format's metadata, like KOReader's purge.
fn rm_item(s: &str) -> String {
    let qs = sq(s);
    let purge = sidecar(s).map(|(ss, ext)| {
        let (qss, m) = (sq(&ss), sq(&format!("{ss}/metadata.{ext}.lua")));
        format!("[ -n \"$f\" ] && [ -d {qss} ] && {{ rm -f {m} {m}.old; rmdir {qss} 2>/dev/null; }}; ")
    }).unwrap_or_default();
    format!("f=; [ -f {qs} ] && f=1; if rm -rf {qs}; then printf 'OK %s\\n' {qs}; {purge}else printf 'ERR %s\\n' {qs}; fi")
}

fn mkdir_item(p: &str) -> String {
    let q = sq(p);
    format!("if [ -e {q} ]; then printf 'EXISTS %s\\n' {q}; elif mkdir {q}; then printf 'OK %s\\n' {q}; else printf 'ERR %s\\n' {q}; fi")
}

/// The items, then a fresh listing of each affected folder, as one script.
fn batch_script(items: &[String], dirs: &[&str]) -> String {
    let mut dirs = dirs.to_vec();
    dirs.sort();
    dirs.dedup();
    let list: Vec<String> = dirs.iter().map(|d| format!("printf '{MARK}%s\\n' {q}; if [ -d {q} ]; then {}; fi", list_cmd(d, "-maxdepth 1"), q = sq(d))).collect();
    format!("{}\n{}\ntrue\n", items.join("\n"), list.join("\n"))
}

/// One ssh call; the script goes on stdin, so a large selection fits.
async fn batch(items: &[String], dirs: &[&str]) -> Res<Change> {
    Ok(parse_change(&ssh::run_script(&batch_script(items, dirs)).await?))
}

#[tauri::command]
pub async fn fs_list(dir: String) -> Res<Vec<Entry>> {
    if !under_us(&dir) { return Err(not_allowed(&dir)) }
    let out = ssh::run(&format!("if [ -d {} ]; then {}; else echo {NODIR}; fi", sq(&dir), list_cmd(&dir, "-maxdepth 1"))).await?;
    if out.trim_end() == NODIR { return Err(tr("This folder doesn't exist anymore.", "Esta carpeta ya no existe.")) }
    Ok(parse_stat(&out).into_iter().map(|x| x.1).collect())
}

/// Every folder under /mnt/us (~5 s); `refresh: false` returns the cached copy.
#[tauri::command]
pub async fn fs_snapshot(app: AppHandle, refresh: bool) -> Res<HashMap<String, Vec<Entry>>> {
    let cache = app.path().app_cache_dir().map_err(|e| e.to_string())?.join("files.json");
    if !refresh {
        return Ok(std::fs::read(&cache).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
    }
    let m = group(parse_stat(&ssh::run(&list_cmd(US, "")).await?));
    std::fs::create_dir_all(cache.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&cache, serde_json::to_vec(&m).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(m)
}

#[tauri::command]
pub async fn fs_rename(from: String, to: String) -> Res<Change> {
    guard(&[&from, &to]).await?;
    batch(&[mv_item(&from, &to)], &[parent(&from), parent(&to)]).await
}

#[tauri::command]
pub async fn fs_move(paths: Vec<String>, dir: String) -> Res<Change> {
    if !under_us(&dir) { return Err(not_allowed(&dir)) }
    let pairs: Vec<(String, String)> = dedupe(&paths).into_iter().map(|p| { let t = format!("{dir}/{}", base(&p)); (p, t) }).collect();
    guard(&pairs.iter().flat_map(|(s, t)| [s.as_str(), t.as_str()]).collect::<Vec<_>>()).await?;
    let mut dirs: Vec<&str> = pairs.iter().map(|(s, _)| parent(s)).collect();
    dirs.push(&dir);
    batch(&pairs.iter().map(|(s, t)| mv_item(s, t)).collect::<Vec<_>>(), &dirs).await
}

#[tauri::command]
pub async fn fs_delete(paths: Vec<String>) -> Res<Change> {
    let paths = dedupe(&paths);
    guard(&paths.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    batch(&paths.iter().map(|p| rm_item(p)).collect::<Vec<_>>(), &paths.iter().map(|p| parent(p)).collect::<Vec<_>>()).await
}

#[tauri::command]
pub async fn fs_mkdir(path: String) -> Res<Change> {
    guard(&[&path]).await?;
    batch(&[mkdir_item(&path)], &[parent(&path)]).await
}

/// A local file or folder into `dir`; refuses to replace an existing item.
#[tauri::command]
pub async fn fs_upload(path: String, dir: String) -> Res<Change> {
    let local = Path::new(&path);
    let name = local.file_name().and_then(|n| n.to_str()).ok_or(tr("invalid file name", "nombre de archivo inválido"))?;
    let target = format!("{dir}/{name}");
    guard(&[&target]).await?;
    let state = |s: &str| vec![Outcome { state: s.into(), path: target.clone() }];
    if ssh::run(&format!("[ -e {} ] && echo y; true", sq(&target))).await?.trim() == "y" {
        return Ok(Change { results: state("EXISTS"), ..Default::default() });
    }
    if local.is_dir() {
        if let Err(err) = ssh::upload_dir(local, &dir).await {
            // the target didn't exist before, so a partial folder is ours to remove
            let _ = ssh::run(&format!("rm -rf {}", sq(&target))).await;
            return Err(err);
        }
    } else {
        ssh::upload(local, &dir).await?;
    }
    let entries = fs_list(dir.clone()).await?;
    Ok(Change { results: state("OK"), dirs: HashMap::from([(dir, entries)]) })
}

/// A file as is, a folder as a .tar, into `dest` on this computer.
#[tauri::command]
pub async fn fs_download(path: String, dest: String) -> Res<u64> {
    if !under_us(&path) || path == US { return Err(not_allowed(&path)) }
    let (p, d, n) = (sq(&path), sq(parent(&path)), sq(&format!("./{}", base(&path))));
    ssh::pull_to_file(&format!("if [ -d {p} ]; then tar -cf - -C {d} {n}; else cat {p}; fi"), Path::new(&dest), |_| {}).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_us_rules() {
        for p in ["/mnt/us", "/mnt/us/books", "/mnt/us/books/a b (1).epub", "/mnt/us/.x"] { assert!(under_us(p), "{p}"); }
        for p in ["/mnt/usx", "/mnt/us/", "/mnt/us/../etc", "/mnt/us/a/./b", "/mnt/us/a//b", "/mnt/us/a\nb", "/etc", "mnt/us/a", ""] {
            assert!(!under_us(p), "{p:?}");
        }
    }

    #[test]
    fn protected_and_read_only() {
        for p in ["/mnt/us", "/mnt/us/documents", "/mnt/us/koreader", "/mnt/us/system", "/mnt/us/koreader/patches",
                  "/mnt/us/koreader/reader.lua", "/mnt/us/koreader/frontend/x.lua", "/mnt/us/koreader/settings", "/mnt/us/../x"] {
            assert!(check(p).is_err(), "{p}");
        }
        for p in ["/mnt/us/books", "/mnt/us/documents/x.pdf", "/mnt/us/koreader/patches/2-x.lua", "/mnt/us/koreader/plugins/a.koplugin/main.lua",
                  "/mnt/us/koreader/data/dict", "/mnt/us/screensavers/a.png"] {
            assert_eq!(check(p), Ok(false), "{p}");
        }
        assert_eq!(check("/mnt/us/koreader/settings/statistics.sqlite3"), Ok(true));
    }

    #[test]
    fn parses_stat() {
        let out = "directory|4096|1700000000|/mnt/us/books/A.sdr\n\
                   regular file|10|1700000001|/mnt/us/books/a|b.epub\n\
                   regular empty file|0|1700000002|/mnt/us/books/first\n\
                   second|line\n\
                   symbolic link|3|1700000003|/mnt/us/l\n\
                   fifo|0|1|/mnt/us/p\n";
        let v = parse_stat(out);
        let kinds: Vec<_> = v.iter().map(|(_, e)| e.kind.as_str()).collect();
        assert_eq!(kinds, ["dir", "file", "file", "link", "file"]);
        assert_eq!(v[1].1.name, "a|b.epub");
        assert_eq!((v[1].1.size, v[1].1.mtime), (10, 1700000001));
        assert_eq!(v[2].0, "/mnt/us/books/first\nsecond|line");
        assert!(!under_us(&v[2].0));
        let m = group(v);
        assert_eq!(m["/mnt/us/books"].len(), 3);
        assert!(m["/mnt/us/books/A.sdr"].is_empty());
        assert_eq!(m["/mnt/us"].len(), 2);
    }

    #[test]
    fn pairs_sidecars() {
        assert_eq!(sidecar("/mnt/us/books/X.epub"), Some(("/mnt/us/books/X.sdr".into(), "epub")));
        assert_eq!(sidecar("/mnt/us/books/X.pdf").unwrap().0, "/mnt/us/books/X.sdr");
        assert_eq!(sidecar("/mnt/us/books/X.v2.epub").unwrap().0, "/mnt/us/books/X.v2.sdr");
        for p in ["/mnt/us/books/README", "/mnt/us/books/.epub", "/mnt/us/books/X.", "/mnt/us/books/X.sdr"] { assert_eq!(sidecar(p), None, "{p}"); }
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(dedupe(&v(&["/b/X.epub", "/b/X.sdr", "/b/Y.sdr"])), v(&["/b/X.epub", "/b/Y.sdr"]));
        assert_eq!(dedupe(&v(&["/b/X.sdr"])), v(&["/b/X.sdr"]));
    }

    #[test]
    fn parses_batch_reply() {
        let c = parse_change("OK /mnt/us/a\nKEPT /mnt/us/a.sdr\nEXISTS /mnt/us/b c\n@@KOB@@ /mnt/us\nregular file|1|2|/mnt/us/z\n@@KOB@@ /mnt/us/e\n");
        assert_eq!(c.results.iter().map(|o| o.state.as_str()).collect::<Vec<_>>(), ["OK", "KEPT", "EXISTS"]);
        assert_eq!(c.results[2].path, "/mnt/us/b c");
        assert_eq!(c.dirs["/mnt/us"][0].name, "z");
        assert!(c.dirs["/mnt/us/e"].is_empty());
    }

    #[test]
    fn large_batch_is_valid_sh() {
        let items: Vec<String> = (0..200).map(|i| format!("/mnt/us/books/Libro {i} (d'Ana).epub")).collect();
        let script = batch_script(&items.iter().map(|p| rm_item(p)).collect::<Vec<_>>(), &["/mnt/us/books", "/mnt/us"]);
        assert!(script.len() > 9000);
        let ok = std::process::Command::new("sh").args(["-n", "-c", &script]).status().unwrap().success();
        assert!(ok);
    }

    #[test]
    fn scripts_are_valid_sh() {
        let cmd = [mv_item("/mnt/us/b/it's.epub", "/mnt/us/c/it's.epub"), rm_item("/mnt/us/b/X.epub"), mkdir_item("/mnt/us/n"),
                   list_cmd("/mnt/us/b", "-maxdepth 1")].join("; ");
        let ok = std::process::Command::new("sh").args(["-n", "-c", &cmd]).status().unwrap().success();
        assert!(ok, "{cmd}");
        assert!(mv_item("/mnt/us/b/X.epub", "/mnt/us/c/X.epub").contains("mv -nT '/mnt/us/b/X.sdr' '/mnt/us/c/X.sdr'"));
        assert!(!mv_item("/mnt/us/b/X.epub", "/mnt/us/b/X.pdf").contains("KEPT"));
    }
}
