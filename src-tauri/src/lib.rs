mod anki;
mod aria;
mod files;
mod highlights;
mod lua;
mod obsidian;
mod setup;
mod ssh;
mod stats;
mod store;

use serde::{Deserialize, Serialize};
use ssh::{sq, tr, Res};
use std::path::{Path, PathBuf};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::Mutex;

const KO: &str = "/mnt/us/koreader";
const BOOKS: &str = "/mnt/us/books";
const FONTS: &str = "/mnt/us/fonts";

fn e<E: ToString>(err: E) -> String { err.to_string() }

fn home() -> PathBuf { PathBuf::from(std::env::var("HOME").unwrap_or_default()) }

// ---------- app settings (JSON in the app config dir) ----------

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Settings {
    backup_dir: String,
    vault_dir: String,
    backup_books: bool,
    auto_backup: bool,
    last_auto_backup: String,
    last_target: Option<Vec<String>>,
    ai_model: String,
    /// UI language, "en" or "es"
    lang: String,
    /// Files view shows KOReader's `.sdr` folders
    show_sdr: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            backup_dir: home().join("Backups/kindle").to_string_lossy().into(),
            vault_dir: home().join("Documents/Obsidian Vault/KOReader").to_string_lossy().into(),
            backup_books: false,
            auto_backup: true,
            last_auto_backup: String::new(),
            last_target: None,
            ai_model: "sonnet".into(),
            lang: "en".into(),
            show_sdr: false,
        }
    }
}

fn settings_path(app: &AppHandle) -> Res<PathBuf> {
    Ok(app.path().app_config_dir().map_err(e)?.join("settings.json"))
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Settings {
    let s: Settings = settings_path(&app).ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    ssh::set_spanish(s.lang == "es");
    s
}

#[tauri::command]
fn set_settings(app: AppHandle, settings: Settings) -> Res<()> {
    ssh::set_spanish(settings.lang == "es");
    let p = settings_path(&app)?;
    std::fs::create_dir_all(p.parent().unwrap()).map_err(e)?;
    std::fs::write(p, serde_json::to_vec_pretty(&settings).map_err(e)?).map_err(e)
}

// ---------- connection ----------

#[derive(Serialize)]
struct Status {
    host: String,
    battery: Option<u8>,
    free_kb: u64,
    total_kb: u64,
    reader_running: bool,
}

const STATUS_CMD: &str = "cat /sys/class/power_supply/*bat*/capacity 2>/dev/null | head -n1; echo; \
     df -k /mnt/us | tail -n1; ps | grep -q '[r]eader.lua' && echo up || echo down";

async fn ensure_target() -> Res<()> {
    if ssh::target().is_none() {
        ssh::set_target(ssh::resolve().await?);
    }
    Ok(())
}

/// Status over the cached IP; on a connect error re-resolve once (network switch, DHCP).
#[tauri::command]
async fn status(app: AppHandle) -> Res<Status> {
    let st = probe().await?;
    // remember the target so the next launch connects without the Match probes
    let target = ssh::target();
    let mut s = get_settings(app.clone());
    if s.last_target != target {
        s.last_target = target;
        let _ = set_settings(app, s);
    }
    Ok(st)
}

async fn probe() -> Res<Status> {
    let had_target = ssh::target().is_some();
    ensure_target().await?;
    let out = match ssh::run(STATUS_CMD).await {
        Err(err) if had_target && ssh::is_connect_error(&err) => {
            ensure_target().await?;
            ssh::run(STATUS_CMD).await?
        }
        r => r?,
    };
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    let battery = lines.first().and_then(|l| l.trim().parse().ok());
    let df = lines.iter().find(|l| l.ends_with("/mnt/us")).map(|l| l.split_whitespace().collect::<Vec<_>>()).unwrap_or_default();
    let n = |i: usize| df.get(i).and_then(|v| v.parse().ok()).unwrap_or(0);
    Ok(Status {
        host: ssh::host(),
        battery,
        total_kb: n(1),
        free_kb: n(3),
        reader_running: lines.last() == Some(&"up"),
    })
}

// ---------- 1. backup ----------

#[derive(Serialize)]
struct BackupFile { name: String, path: String, size: u64, modified: u64 }

#[tauri::command]
async fn backup_run(app: AppHandle, stamp: String, progress: Channel<u64>) -> Res<BackupFile> {
    backup_to(app, &stamp, &[], |n| { let _ = progress.send(n); }).await
}

/// `extra`: more paths under /mnt/us to include, e.g. the plugin folder the Store is about to replace.
async fn backup_to(app: AppHandle, stamp: &str, extra: &[String], on: impl FnMut(u64)) -> Res<BackupFile> {
    if !stamp.chars().all(|c| c.is_ascii_digit() || c == '-') { return Err("stamp inválido".into()); }
    let s = get_settings(app);
    let dir = PathBuf::from(&s.backup_dir);
    std::fs::create_dir_all(&dir).map_err(e)?;
    let cmd = format!("{} && tar -cf - -T /tmp/kob-list; r=$?; rm -f /tmp/kob-list; exit $r", backup_list_cmd(s.backup_books, extra));
    let dest = dir.join(format!("kindle-{stamp}.tar"));
    ssh::pull_to_file(&cmd, &dest, on).await?;
    Ok(file_info(&dest).ok_or(tr("couldn't read the backup", "no se pudo leer el respaldo"))?)
}

/// Shell that writes the backup's path list to /tmp/kob-list, run from /mnt/us.
fn backup_list_cmd(books: bool, more: &[String]) -> String {
    let (sdr_roots, books_dir) = if books { ("documents", "books") } else { ("books documents", "") };
    let extra = format!("{books_dir} {}", more.iter().map(|p| sq(p)).collect::<Vec<_>>().join(" "));
    format!(
        "cd /mnt/us && {{ for p in koreader/settings koreader/settings.reader.lua koreader/history.lua \
         koreader/defaults.custom.lua koreader/patches koreader/styletweaks {extra}; do [ -e \"$p\" ] && echo \"$p\"; done; \
         find {sdr_roots} -type d -name '*.sdr' ! -path '*/dictionaries/*' 2>/dev/null; }} > /tmp/kob-list")
}

#[derive(Serialize)]
struct BackupSize { books_bytes: u64, books_files: u64, base_bytes: u64, full_bytes: u64, bytes_per_sec: f64 }

/// Real sizes from `du -sk` on the Kindle plus a measured transfer speed (8 MB over the same ssh path).
#[tauri::command]
async fn backup_size() -> Res<BackupSize> {
    let du = "tr '\\n' '\\000' < /tmp/kob-list | xargs -0 du -sk | awk '{s+=$1} END {print s+0}'";
    let out = ssh::run(&format!(
        "{base} && {du} && {full} && {du} && du -sk /mnt/us/books | cut -f1 && find /mnt/us/books -type f | wc -l; rm -f /tmp/kob-list",
        base = backup_list_cmd(false, &[]), full = backup_list_cmd(true, &[]))).await?;
    let n: Vec<u64> = out.split_whitespace().filter_map(|v| v.parse().ok()).collect();
    let [base_kb, full_kb, books_kb, books_files] = n[..] else { return Err(tr(format!("unexpected du output: {out}"), format!("respuesta inesperada de du: {out}"))) };

    let probe = std::env::temp_dir().join("kob-speed.bin");
    let t = std::time::Instant::now();
    let bytes = ssh::pull_to_file("dd if=/dev/zero bs=65536 count=128 2>/dev/null", &probe, |_| {}).await?;
    let secs = t.elapsed().as_secs_f64();
    let _ = std::fs::remove_file(probe);
    Ok(BackupSize {
        books_bytes: books_kb * 1024, books_files, base_bytes: base_kb * 1024, full_bytes: full_kb * 1024,
        bytes_per_sec: bytes as f64 / secs.max(0.001),
    })
}

fn file_info(p: &Path) -> Option<BackupFile> {
    let m = std::fs::metadata(p).ok()?;
    Some(BackupFile {
        name: p.file_name()?.to_string_lossy().into(),
        path: p.to_string_lossy().into(),
        size: m.len(),
        modified: m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs(),
    })
}

// commands that touch the disk or spawn processes are async, so they run off the main (UI) thread
#[tauri::command]
async fn backup_list(app: AppHandle) -> Vec<BackupFile> {
    let dir = PathBuf::from(get_settings(app).backup_dir);
    let mut v: Vec<BackupFile> = std::fs::read_dir(dir).into_iter().flatten().flatten()
        .map(|d| d.path())
        .filter(|p| p.extension().is_some_and(|x| x == "tar"))
        .filter_map(|p| file_info(&p))
        .collect();
    v.sort_by(|a, b| b.modified.cmp(&a.modified));
    v
}

#[tauri::command]
fn reveal(path: String) -> Res<()> {
    if cfg!(target_os = "macos") { return open_with(&["-R", &path]); }
    // xdg-open can't select a file, so open its folder
    let p = Path::new(&path);
    let dir = if p.is_dir() { p } else { p.parent().unwrap_or(p) };
    open_with(&[&dir.to_string_lossy()])
}

/// A catalog item's GitHub page in the browser.
#[tauri::command]
fn open_url(url: String) -> Res<()> {
    if !url.starts_with("https://github.com/") { return Err(format!("url not allowed: {url}")); }
    open_with(&[&url])
}

/// `open` on macOS, `xdg-open` on Linux.
fn open_with(args: &[&str]) -> Res<()> {
    let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(cmd).args(args).spawn().map_err(e).map(|_| ())
}

// ---------- 2. reading stats ----------

#[tauri::command]
async fn stats(app: AppHandle, refresh: bool) -> Res<Option<stats::Stats>> {
    let dir = app.path().app_cache_dir().map_err(e)?;
    std::fs::create_dir_all(&dir).map_err(e)?;
    let db = dir.join("statistics.sqlite3");
    if refresh {
        // .backup gives a consistent copy even while KOReader is writing
        let cmd = format!("sqlite3 {KO}/settings/statistics.sqlite3 '.backup /tmp/kob-stats.db' && cat /tmp/kob-stats.db; r=$?; rm -f /tmp/kob-stats.db; exit $r");
        ssh::pull_to_file(&cmd, &db, |_| {}).await?;
    }
    let Some(info) = file_info(&db) else { return Ok(None) };
    tauri::async_runtime::spawn_blocking(move || stats::read(&db, info.modified as i64)).await.map_err(e)?.map(Some).map_err(e)
}

// ---------- 3. highlights ----------

#[derive(Serialize, Deserialize, Default)]
struct HighlightSet { books: Vec<highlights::Book>, errors: Vec<String>, fetched_at: u64 }

#[tauri::command]
async fn highlights(app: AppHandle, refresh: bool) -> Res<HighlightSet> {
    let cache = app.path().app_cache_dir().map_err(e)?.join("highlights.json");
    if !refresh {
        return Ok(std::fs::read(&cache).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
    }
    let dump = ssh::run(highlights::FETCH_CMD).await?;
    let (books, errors) = highlights::parse_dump(&dump);
    let fetched_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(e)?.as_secs();
    let set = HighlightSet { books, errors, fetched_at };
    std::fs::create_dir_all(cache.parent().unwrap()).map_err(e)?;
    std::fs::write(&cache, serde_json::to_vec(&set).map_err(e)?).map_err(e)?;
    Ok(set)
}

#[derive(Serialize, Default)]
struct ExportReport { created: usize, updated: usize, added: usize, legacy: usize, dir: String }

fn fmt_secs(s: i64) -> String {
    let m = (s + 30) / 60;
    if m < 60 { format!("{m} min") } else { format!("{} h {} min", m / 60, m % 60) }
}

type Extras = Vec<(String, String, obsidian::Extra)>;

/// Reading data per book from the cached stats snapshot; empty when there is none yet.
fn extras(app: &AppHandle) -> Extras {
    let Ok(dir) = app.path().app_cache_dir() else { return vec![] };
    stats::per_book(&dir.join("statistics.sqlite3")).unwrap_or_default().into_iter()
        .map(|(md5, title, day, secs)| (md5, title, obsidian::Extra {
            last_read: (!day.is_empty()).then_some(day),
            read_time: (secs > 0).then(|| fmt_secs(secs)),
        }))
        .collect()
}

fn extra_for(all: &Extras, b: &highlights::Book) -> obsidian::Extra {
    b.md5.as_ref().and_then(|m| all.iter().find(|(x, _, _)| x == m))
        .or_else(|| all.iter().find(|(_, t, _)| *t == b.title))
        .map(|(_, _, x)| x.clone())
        .unwrap_or_default()
}

/// Writes or merges one note per book into `vault_dir`; returns the note paths in `books` order.
fn export(app: &AppHandle, books: &[highlights::Book], with_index: bool) -> Res<(ExportReport, Vec<PathBuf>)> {
    let dir = PathBuf::from(get_settings(app.clone()).vault_dir);
    std::fs::create_dir_all(&dir).map_err(e)?;
    let all = extras(app);
    let mut r = ExportReport { dir: dir.to_string_lossy().into(), ..Default::default() };
    let mut paths = vec![];
    let mut rows = vec![];
    for b in books {
        let x = extra_for(&all, b);
        let name = highlights::file_name(&b.title);
        let p = dir.join(&name);
        let write = |doc: &str| std::fs::write(&p, doc).map_err(|err| tr(format!("Couldn't write {}: {err}", p.display()), format!("No pude escribir {}: {err}", p.display())));
        match std::fs::read_to_string(&p) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                write(&obsidian::render(b, &x))?;
                r.created += 1;
                r.added += b.notes.len();
            }
            Err(err) => return Err(tr(format!("Couldn't read {}: {err}", p.display()), format!("No pude leer {}: {err}", p.display()))),
            Ok(old) => match obsidian::merge(&old, b, &x) {
                obsidian::Merge::Unchanged => {}
                obsidian::Merge::Updated { doc, added } => {
                    write(&doc)?;
                    r.updated += 1;
                    r.added += added;
                }
                obsidian::Merge::Legacy => {
                    // a note from 0.1.1 has no block ids: keep it next to the new one
                    let old_p = dir.join(format!("{} (anterior).md", name.trim_end_matches(".md")));
                    if old_p.exists() { return Err(tr(format!("{} already exists. Move it and export again.", old_p.display()), format!("Ya existe {}; movelo y exportá de nuevo.", old_p.display()))); }
                    std::fs::rename(&p, &old_p).map_err(e)?;
                    write(&obsidian::render(b, &x))?;
                    r.legacy += 1;
                    r.added += b.notes.len();
                }
            },
        }
        rows.push((name.trim_end_matches(".md").to_string(), b, x));
        paths.push(p);
    }
    if with_index {
        std::fs::write(dir.join("Libros.md"), obsidian::index(&rows)).map_err(e)?;
    }
    Ok((r, paths))
}

#[tauri::command]
async fn highlights_export(app: AppHandle, books: Vec<highlights::Book>) -> Res<ExportReport> {
    Ok(export(&app, &books, true)?.0)
}

#[tauri::command]
fn obsidian_vaults() -> Vec<String> {
    let rel = if cfg!(target_os = "macos") { "Library/Application Support/obsidian/obsidian.json" } else { ".config/obsidian/obsidian.json" };
    let json = std::fs::read_to_string(home().join(rel)).unwrap_or_default();
    obsidian::vaults(&json).into_iter().filter(|p| Path::new(p).is_dir()).collect()
}

/// Creates ~/Documents/KOReader Vault and asks Obsidian to open it, which registers it.
#[tauri::command]
fn obsidian_create_vault() -> Res<String> {
    let v = home().join("Documents/KOReader Vault");
    std::fs::create_dir_all(v.join(".obsidian")).map_err(e)?;
    let path = v.to_string_lossy().to_string();
    open_with(&[&format!("obsidian://open?path={}", obsidian::url_encode(&path))])?;
    Ok(path)
}

#[derive(Default)]
struct Enrich { child: Mutex<Option<tokio::process::Child>>, cancel: AtomicBool }

#[derive(Serialize, Clone)]
struct EnrichEvent { book: String, kind: &'static str, text: String }

/// Exports `books`, then runs `claude -p` on each note, one at a time.
#[tauri::command]
async fn enrich_start(app: AppHandle, job: State<'_, Enrich>, books: Vec<highlights::Book>, model: String, today: String,
                      events: Channel<EnrichEvent>) -> Res<()> {
    if !obsidian::MODELS.contains(&model.as_str()) { return Err(format!("modelo desconocido: {model}")); }
    if today.len() != 10 || !today.chars().all(|c| c.is_ascii_digit() || c == '-') { return Err("fecha inválida".into()); }
    let claude = obsidian::find_claude(&home()).ok_or(tr("Claude Code isn't installed. Install it with `brew install --cask claude-code`.", "No encontré Claude Code. Instalalo con `brew install --cask claude-code`."))?;
    let (_, paths) = export(&app, &books, false)?;
    let root = obsidian::vault_root(&PathBuf::from(get_settings(app.clone()).vault_dir));
    let git = root.join(".git").exists();
    job.cancel.store(false, SeqCst);
    let send = |book: &str, kind, text: String| { let _ = events.send(EnrichEvent { book: book.into(), kind, text }); };
    for (b, p) in books.iter().zip(&paths) {
        if job.cancel.load(SeqCst) { break; }
        send(&b.title, "start", String::new());
        if !git { let _ = std::fs::copy(p, p.with_extension("md.kob.bak")); }
        match enrich_one(&claude, &root, p, &model, &job, |t| send(&b.title, "tool", t)).await {
            Ok(()) => {
                let doc = std::fs::read_to_string(p).map_err(e)?;
                std::fs::write(p, obsidian::set_fm(&doc, "enriched", &today)).map_err(e)?;
                send(&b.title, "done", String::new());
            }
            Err(err) => send(&b.title, "error", err),
        }
    }
    Ok(())
}

async fn enrich_one(claude: &Path, root: &Path, note: &Path, model: &str, job: &Enrich, mut tool: impl FnMut(String)) -> Res<()> {
    let rel = note.strip_prefix(root).unwrap_or(note).to_string_lossy().to_string();
    let mut child = tokio::process::Command::new(claude)
        .current_dir(root)
        .arg("-p").arg(obsidian::prompt(&rel))
        .args(["--model", model, "--output-format", "stream-json", "--verbose",
               "--tools", "Read,Glob,Grep,Edit", "--permission-mode", "acceptEdits",
               "--strict-mcp-config", "--setting-sources", "project", "--no-session-persistence", "--disable-slash-commands"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn().map_err(e)?;
    let out = child.stdout.take().ok_or("sin salida")?;
    let mut err = child.stderr.take().ok_or("sin salida")?;
    let stderr = tauri::async_runtime::spawn(async move { let mut s = String::new(); let _ = err.read_to_string(&mut s).await; s });
    *job.child.lock().await = Some(child);
    let mut result = None;
    let mut lines = BufReader::new(out).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        match obsidian::event(&l) {
            Some(obsidian::Event::Tool(t)) => tool(t),
            Some(obsidian::Event::Done) => result = Some(Ok(())),
            Some(obsidian::Event::Failed(m)) => result = Some(Err(m)),
            None => {}
        }
    }
    let child = job.child.lock().await.take();
    let (Some(mut child), false) = (child, job.cancel.load(SeqCst)) else { return Err(tr("Canceled", "Cancelado")) };
    let status = child.wait().await.map_err(e)?;
    let stderr = stderr.await.unwrap_or_default();
    result.unwrap_or_else(|| Err(stderr.trim().lines().last().map(str::to_string).unwrap_or(tr(format!("claude exited with {status}"), format!("claude terminó con {status}")))))
}

/// Asks Claude for cards per book and writes one Anki text file to ~/Downloads; returns its path.
#[tauri::command]
async fn anki_start(app: AppHandle, job: State<'_, Enrich>, books: Vec<highlights::Book>, model: String, today: String,
                    events: Channel<EnrichEvent>) -> Res<Option<String>> {
    if !obsidian::MODELS.contains(&model.as_str()) { return Err(format!("modelo desconocido: {model}")); }
    if today.len() != 10 || !today.chars().all(|c| c.is_ascii_digit() || c == '-') { return Err("fecha inválida".into()); }
    let claude = obsidian::find_claude(&home()).ok_or(tr("Claude Code isn't installed. Install it with `brew install --cask claude-code`.", "No encontré Claude Code. Instalalo con `brew install --cask claude-code`."))?;
    // an empty cwd, so Claude Code picks up no project CLAUDE.md
    let cwd = app.path().app_cache_dir().map_err(e)?.join("anki");
    std::fs::create_dir_all(&cwd).map_err(e)?;
    job.cancel.store(false, SeqCst);
    let send = |book: &str, kind, text: String| { let _ = events.send(EnrichEvent { book: book.into(), kind, text }); };
    let mut done = vec![];
    for b in &books {
        if job.cancel.load(SeqCst) { break; }
        send(&b.title, "start", String::new());
        match anki_one(&claude, &cwd, b, &model, &job).await {
            Ok(cards) if cards.is_empty() => send(&b.title, "error", tr("Claude wrote no cards", "Claude no escribió tarjetas")),
            Ok(cards) => { send(&b.title, "done", tr(format!("{} cards", cards.len()), format!("{} tarjetas", cards.len()))); done.push((b, cards)); }
            Err(err) => send(&b.title, "error", err),
        }
    }
    if done.is_empty() { return Ok(None); }
    // never overwrite a file from an earlier run that may not be imported yet
    let dl = home().join("Downloads");
    let p = (1..).map(|n| dl.join(if n == 1 { format!("KOReader Anki {today}.txt") } else { format!("KOReader Anki {today} ({n}).txt") }))
        .find(|p| !p.exists()).unwrap();
    std::fs::write(&p, anki::tsv(&done)).map_err(|err| tr(format!("Couldn't write {}: {err}", p.display()), format!("No pude escribir {}: {err}", p.display())))?;
    Ok(Some(p.to_string_lossy().into()))
}

async fn anki_one(claude: &Path, cwd: &Path, b: &highlights::Book, model: &str, job: &Enrich) -> Res<Vec<anki::Card>> {
    let mut child = tokio::process::Command::new(claude)
        .current_dir(cwd)
        .arg("-p").arg(anki::prompt(b))
        .args(["--model", model, "--output-format", "json", "--json-schema", anki::SCHEMA, "--tools", "",
               "--strict-mcp-config", "--setting-sources", "project", "--no-session-persistence", "--disable-slash-commands"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn().map_err(e)?;
    let mut out = child.stdout.take().ok_or("sin salida")?;
    let mut err = child.stderr.take().ok_or("sin salida")?;
    let stderr = tauri::async_runtime::spawn(async move { let mut s = String::new(); let _ = err.read_to_string(&mut s).await; s });
    *job.child.lock().await = Some(child);
    let mut stdout = String::new();
    let _ = out.read_to_string(&mut stdout).await;
    let child = job.child.lock().await.take();
    let (Some(mut child), false) = (child, job.cancel.load(SeqCst)) else { return Err(tr("Canceled", "Cancelado")) };
    let status = child.wait().await.map_err(e)?;
    if stdout.trim().is_empty() {
        let stderr = stderr.await.unwrap_or_default();
        return Err(stderr.trim().lines().last().map(str::to_string).unwrap_or(tr(format!("claude exited with {status}"), format!("claude terminó con {status}"))));
    }
    anki::parse(&stdout, &b.notes)
}

/// Opens the file in Anki, which shows its import dialog.
#[tauri::command]
async fn anki_open(path: String) -> Res<()> {
    let ok = if cfg!(target_os = "macos") {
        tokio::process::Command::new("open").args(["-a", "Anki", &path]).status().await.map_err(e)?.success()
    } else {
        // Anki on Linux imports a file passed on its command line
        std::process::Command::new("anki").arg(&path).spawn().is_ok()
    };
    if ok { Ok(()) } else { Err(tr("Anki isn't installed. Get it from apps.ankiweb.net and open it once to create your profile.", "No encontré Anki. Instalalo desde apps.ankiweb.net y abrilo una vez para crear tu perfil.")) }
}

// ---------- SSH setup (Settings view) ----------

#[tauri::command]
async fn ssh_setup_info() -> setup::Info { setup::info(&home()).await }

#[tauri::command]
async fn ssh_create_key() -> Res<()> { setup::create_key(&home()).await }

#[tauri::command]
fn ssh_add_host(ip: String, port: u16) -> Res<()> {
    setup::add_host(&home(), &ip, port)?;
    ssh::set_target(None);
    Ok(())
}

#[tauri::command]
async fn ssh_install_usb() -> Res<String> { setup::install_usb(&home()) }

#[tauri::command]
async fn ssh_test() -> Res<String> {
    let v = setup::test().await?;
    ssh::set_target(None);
    Ok(v)
}

#[tauri::command]
fn open_ssh_config() -> Res<()> {
    let p = home().join(".ssh/config");
    if !p.exists() { return Err(tr("~/.ssh/config doesn't exist yet. Add the Kindle first.", "~/.ssh/config todavía no existe. Primero agregá la Kindle.")); }
    let p = p.to_string_lossy().to_string();
    if cfg!(target_os = "macos") { open_with(&["-t", &p]) } else { open_with(&[&p]) }
}

#[tauri::command]
async fn enrich_stop(job: State<'_, Enrich>) -> Res<()> {
    job.cancel.store(true, SeqCst);
    if let Some(mut c) = job.child.lock().await.take() { let _ = c.kill().await; }
    Ok(())
}

// ---------- 4 & 8. send books / fonts ----------

fn remote_dir(kind: &str) -> Res<&'static str> {
    match kind {
        "books" => Ok(BOOKS),
        "fonts" => Ok(FONTS),
        "patches" => Ok("/mnt/us/koreader/patches"),
        _ => Err(format!("destino desconocido: {kind}")),
    }
}

#[tauri::command]
async fn send_file(path: String, kind: String) -> Res<String> {
    ssh::upload(Path::new(&path), remote_dir(&kind)?).await
}

#[derive(Serialize)]
struct Entry { name: String, size: u64 }

#[tauri::command]
async fn list_dir(kind: String) -> Res<Vec<Entry>> {
    let dir = remote_dir(&kind)?;
    let out = ssh::run(&format!(
        "cd {} 2>/dev/null && stat -c '%s %n' * 2>/dev/null; true", sq(dir))).await?;
    let mut v: Vec<Entry> = out.lines().filter_map(|l| {
        let (size, name) = l.trim().split_once(' ')?;
        Some(Entry { name: name.to_string(), size: size.parse().ok()? })
    }).collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(v)
}

// ---------- 5 & 6. remote text files (patches, settings) ----------

/// Only files the app manages; never a path the UI could steer elsewhere.
fn allowed(path: &str) -> Res<()> {
    let ok_dir = |dir: &str| path.strip_prefix(dir).and_then(|r| r.strip_prefix('/'))
        .is_some_and(|name| !name.is_empty() && !name.contains('/') && name != "." && name != "..");
    let fixed = [format!("{KO}/defaults.custom.lua"), format!("{KO}/settings.reader.lua")];
    if fixed.iter().any(|f| f == path) || ok_dir(&format!("{KO}/patches")) || ok_dir(FONTS) {
        Ok(())
    } else {
        Err(tr(format!("path not allowed: {path}"), format!("ruta no permitida: {path}")))
    }
}

async fn reader_running() -> Res<bool> {
    Ok(ssh::run("ps | grep -q '[r]eader.lua' && echo up || echo down").await?.trim() == "up")
}

#[tauri::command]
async fn read_remote(path: String) -> Res<String> {
    allowed(&path)?;
    ssh::run(&format!("[ -f {p} ] && cat {p} || true", p = sq(&path))).await
}

#[tauri::command]
async fn write_remote(path: String, content: String) -> Res<()> {
    allowed(&path)?;
    if path.ends_with("settings.reader.lua") && reader_running().await? {
        return Err(tr("KOReader is open and would overwrite this file when it exits. Close it on the Kindle and save again.", "KOReader está abierto y sobrescribiría este archivo al cerrar. Cerralo en la Kindle y volvé a guardar."));
    }
    let p = sq(&path);
    ssh::run_with_input(&format!("[ -f {p} ] && cp {p} {p}.kob.bak; cat > {p}.kob-part && mv {p}.kob-part {p}"), content.as_bytes()).await
}

#[tauri::command]
async fn rename_remote(from: String, to: String) -> Res<()> {
    allowed(&from)?;
    allowed(&to)?;
    ssh::run(&format!("mv {} {}", sq(&from), sq(&to))).await.map(|_| ())
}

#[tauri::command]
async fn delete_remote(path: String) -> Res<()> {
    allowed(&path)?;
    ssh::run(&format!("rm -f {}", sq(&path))).await.map(|_| ())
}

// ---------- 9. store (Storefront catalog) ----------

static STORE_BUSY: AtomicBool = AtomicBool::new(false);

/// The catalog, from the cache when it was checked less than an hour ago or the download fails.
#[tauri::command]
async fn store_catalog(app: AppHandle, refresh: bool) -> Res<serde_json::Value> {
    let dir = app.path().app_cache_dir().map_err(e)?.join("store");
    let (file, checked) = (dir.join("catalog.json"), dir.join("catalog.checked"));
    let age = std::fs::metadata(&checked).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
    let mut offline = None;
    if refresh || !file.exists() || age.is_none_or(|a| a.as_secs() > 3600) {
        // a cut download never replaces the copy the offline fallback reads
        let fresh = aria::download(store::CATALOG_URL, &dir, "catalog.new.json", |_, _| {}).await.and_then(|p| {
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&p).map_err(e)?).map_err(e)?;
            std::fs::rename(&p, &file).map_err(e)
        });
        match fresh {
            Ok(()) => { let _ = std::fs::write(&checked, b""); }
            Err(err) if file.exists() => offline = Some(err),
            Err(err) => return Err(err),
        }
    }
    let c: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).map_err(e)?).map_err(e)?;
    let mut v = store::slim(&c);
    v["offline"] = offline.into();
    Ok(v)
}

fn cached_catalog(app: &AppHandle) -> Res<serde_json::Value> {
    let p = app.path().app_cache_dir().map_err(e)?.join("store/catalog.json");
    serde_json::from_slice(&std::fs::read(p).map_err(|_| tr("Open the Store once to download the catalog.", "Abrí la Tienda una vez para bajar el catálogo."))?).map_err(e)
}

#[derive(Serialize)]
struct StoreState { items: Vec<store::Owned>, registry_error: Option<String> }

#[tauri::command]
async fn store_installed(app: AppHandle) -> Res<StoreState> {
    let c = cached_catalog(&app)?;
    let r = store::parse_list(&ssh::run(store::LIST_CMD).await?);
    Ok(StoreState { items: store::owned(&c, &r), registry_error: r.registry.as_ref().err().cloned() })
}

/// Clears STORE_BUSY when the operation ends, however it ends.
struct Busy;
impl Drop for Busy { fn drop(&mut self) { STORE_BUSY.store(false, SeqCst); } }

async fn store_guard() -> Res<Busy> {
    if STORE_BUSY.swap(true, SeqCst) { return Err(tr("Another store operation is running.", "Ya hay otra operación de la tienda en curso.")); }
    let busy = Busy;
    if reader_running().await? {
        return Err(tr("KOReader is open, and it would overwrite Storefront's list of installs when it exits. Close it on the Kindle and try again.",
                      "KOReader está abierto y sobrescribiría la lista de instalaciones de Storefront al cerrar. Cerralo en la Kindle y volvé a intentar."));
    }
    Ok(busy)
}

/// Full backup, then install or update one catalog item.
#[tauri::command]
async fn store_install(app: AppHandle, kind: String, id: String, stamp: String, progress: Channel<store::Step>) -> Res<()> {
    let _busy = store_guard().await?;
    let c = cached_catalog(&app)?;
    let extra = store::targets(&c, &kind, &id, &store::remote_registry().await.unwrap_or_default());
    backup_to(app.clone(), &stamp, &extra, |n| { let _ = progress.send(store::Step { step: "backup", done: n, total: 0 }); }).await?;
    let work = app.path().app_cache_dir().map_err(e)?.join("store/work");
    store::install(&c, &kind, &id, &work, |s| { let _ = progress.send(s); }).await?;
    let _ = progress.send(store::Step { step: "done", done: 0, total: 0 });
    Ok(())
}

/// Full backup, then remove one item that a store installed.
#[tauri::command]
async fn store_uninstall(app: AppHandle, kind: String, id: String, stamp: String, progress: Channel<store::Step>) -> Res<()> {
    let _busy = store_guard().await?;
    let c = cached_catalog(&app).unwrap_or_default();
    let extra = store::targets(&c, &kind, &id, &store::remote_registry().await?);
    backup_to(app, &stamp, &extra, |n| { let _ = progress.send(store::Step { step: "backup", done: n, total: 0 }); }).await?;
    let _ = progress.send(store::Step { step: "send", done: 0, total: 0 });
    store::uninstall(&kind, &id).await?;
    let _ = progress.send(store::Step { step: "done", done: 0, total: 0 });
    Ok(())
}

// ---------- 7. live log + screen message ----------

#[derive(Default)]
struct LogTail(Mutex<Option<tokio::process::Child>>);

#[tauri::command]
async fn log_start(tail: State<'_, LogTail>, lines: Channel<String>) -> Res<()> {
    let mut slot = tail.0.lock().await;
    if let Some(mut old) = slot.take() { let _ = old.kill().await; }
    let mut child = ssh::stream(&format!("tail -n 200 -f {KO}/crash.log"))?;
    let out = child.stdout.take().ok_or("sin salida")?;
    *slot = Some(child);
    tauri::async_runtime::spawn(async move {
        let mut r = BufReader::new(out).lines();
        while let Ok(Some(l)) = r.next_line().await {
            if lines.send(l).is_err() { break; }
        }
    });
    Ok(())
}

#[tauri::command]
async fn log_stop(tail: State<'_, LogTail>) -> Res<()> {
    if let Some(mut c) = tail.0.lock().await.take() { let _ = c.kill().await; }
    Ok(())
}

#[tauri::command]
async fn screen_message(text: String) -> Res<()> {
    let text: String = text.chars().filter(|c| !c.is_control()).take(200).collect();
    if text.trim().is_empty() { return Err(tr("The message is empty", "El mensaje está vacío")); }
    ssh::run(&format!("{KO}/fbink -q -m -M -S 3 {}", sq(&text))).await.map(|_| ())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            ssh::set_target(get_settings(app.handle().clone()).last_target);
            Ok(())
        })
        .manage(LogTail::default())
        .manage(Enrich::default())
        .invoke_handler(tauri::generate_handler![
            get_settings, set_settings, status,
            backup_run, backup_list, backup_size, reveal, open_url,
            stats,
            highlights, highlights_export, obsidian_vaults, obsidian_create_vault, enrich_start, enrich_stop, anki_start, anki_open,
            ssh_setup_info, ssh_create_key, ssh_add_host, ssh_install_usb, ssh_test, open_ssh_config,
            send_file, list_dir,
            read_remote, write_remote, rename_remote, delete_remote,
            log_start, log_stop, screen_message,
            store_catalog, store_installed, store_install, store_uninstall,
            files::fs_list, files::fs_snapshot, files::fs_rename, files::fs_move, files::fs_delete, files::fs_mkdir, files::fs_upload, files::fs_download,
        ])
        .run(tauri::generate_context!())
        .expect("error while running KOBridge");
}

#[cfg(test)]
mod tests {
    use super::allowed;

    #[test]
    fn only_managed_paths() {
        assert!(allowed("/mnt/us/koreader/patches/2-x.lua").is_ok());
        assert!(allowed("/mnt/us/koreader/settings.reader.lua").is_ok());
        assert!(allowed("/mnt/us/fonts/Inter Bold.ttf").is_ok());
        assert!(allowed("/mnt/us/koreader/patches/../reader.lua").is_err());
        assert!(allowed("/mnt/us/koreader/patches/").is_err());
        assert!(allowed("/mnt/us/fonts/a/b.ttf").is_err());
        assert!(allowed("/etc/passwd").is_err());
    }
}

/// Against the real Kindle: `cargo test -- --ignored --nocapture`
#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore]
    fn kindle_end_to_end() {
        tauri::async_runtime::block_on(async {
            let s = probe().await.expect("status");
            println!("host {} battery {:?} free {} MB reader {}", s.host, s.battery, s.free_kb / 1024, s.reader_running);

            let dump = ssh::run(highlights::FETCH_CMD).await.expect("fetch");
            let (books, errors) = highlights::parse_dump(&dump);
            println!("{} books with highlights, {} parse errors {:?}", books.len(), errors.len(), errors.first());
            assert!(errors.is_empty());
            let top = books.iter().max_by_key(|b| b.notes.len()).unwrap();
            let md = obsidian::render(top, &obsidian::Extra::default());
            println!("most: {} ({}) md5 {:?}\n{}", top.title, top.notes.len(), top.md5, &md[..md.char_indices().nth(600).map_or(md.len(), |(i, _)| i)]);

            let tmp = std::env::temp_dir().join("kob-live-stats.db");
            let cmd = format!("sqlite3 {KO}/settings/statistics.sqlite3 '.backup /tmp/kob-stats.db' && cat /tmp/kob-stats.db; r=$?; rm -f /tmp/kob-stats.db; exit $r");
            ssh::pull_to_file(&cmd, &tmp, |_| {}).await.expect("stats pull");
            let st = stats::read(&tmp, 0).expect("stats read");
            println!("stats: {:.1} h, {} days, {} books, last day {:?} min", st.total_hours, st.days_read, st.books, st.daily.last().map(|d| d.minutes));
            assert_eq!(st.daily.len(), 30);

            let fonts = list_dir("fonts".into()).await.expect("fonts");
            let patches = list_dir("patches".into()).await.expect("patches");
            println!("fonts {} · patches {:?}", fonts.len(), patches.iter().map(|p| &p.name).collect::<Vec<_>>());
            let txt = read_remote(format!("{KO}/patches/{}", patches[0].name)).await.expect("read patch");
            assert_eq!(txt.len() as u64, patches[0].size);
        });
    }

    #[test]
    #[ignore]
    fn kindle_writes() {
        tauri::async_runtime::block_on(async {
            // backup: same remote command as backup_run, settings + sidecars only
            let dir = std::env::temp_dir().join("kob-live-backup");
            std::fs::create_dir_all(&dir).unwrap();
            let cmd = "cd /mnt/us && { for p in koreader/settings koreader/settings.reader.lua koreader/history.lua \
                 koreader/defaults.custom.lua koreader/patches koreader/styletweaks ; do [ -e \"$p\" ] && echo \"$p\"; done; \
                 find books documents -type d -name '*.sdr' ! -path '*/dictionaries/*' 2>/dev/null; } > /tmp/kob-list && \
                 tar -cf - -T /tmp/kob-list; r=$?; rm -f /tmp/kob-list; exit $r";
            let tar = dir.join("t.tar");
            let n = ssh::pull_to_file(cmd, &tar, |_| {}).await.expect("backup");
            let listing = std::process::Command::new("tar").arg("-tf").arg(&tar).output().unwrap();
            let names = String::from_utf8_lossy(&listing.stdout);
            println!("backup {} MB, {} entries", n / 1024 / 1024, names.lines().count());
            assert!(names.contains("koreader/settings/statistics.sqlite3"));
            assert!(names.contains(".sdr/metadata.epub.lua"));

            // upload a file whose name has spaces, parens and a quote
            let local = dir.join("Prueba (1) d'Ana.epub");
            std::fs::write(&local, b"hola").unwrap();
            let remote = ssh::upload(&local, "/tmp/kob test").await.expect("upload");
            assert_eq!(ssh::run(&format!("cat {}", sq(&remote))).await.unwrap(), "hola");
            ssh::run("rm -rf '/tmp/kob test'").await.unwrap();

            // write / rename / delete a disabled patch
            let p = format!("{KO}/patches/9-kob-test.lua.disabled");
            write_remote(p.clone(), "-- kob \"test\" 'q'\n".into()).await.expect("write");
            assert_eq!(read_remote(p.clone()).await.unwrap(), "-- kob \"test\" 'q'\n");
            let q = format!("{KO}/patches/9-kob-test2.lua.disabled");
            rename_remote(p.clone(), q.clone()).await.expect("rename");
            delete_remote(q.clone()).await.expect("delete");
            delete_remote(format!("{p}.kob.bak")).await.ok();
            assert!(!ssh::run(&format!("ls {KO}/patches")).await.unwrap().contains("kob-test"));

            // settings.reader.lua is refused while KOReader runs
            if probe().await.unwrap().reader_running {
                assert!(write_remote(format!("{KO}/settings.reader.lua"), String::new()).await.is_err());
            }

            // streaming
            let mut child = ssh::stream(&format!("tail -n 3 {KO}/crash.log")).unwrap();
            let out = child.stdout.take().unwrap();
            let mut lines = BufReader::new(out).lines();
            let mut got = 0;
            while let Ok(Some(_)) = lines.next_line().await { got += 1; }
            assert_eq!(got, 3);
            std::fs::remove_dir_all(&dir).ok();
            println!("writes ok");
        });
    }

    /// Real highlights from the app cache into a temp folder, twice: `cargo test export_cache -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn export_cache() {
        let cache = home().join("Library/Caches/com.danisnowman.kobridge/highlights.json");
        let set: HighlightSet = serde_json::from_slice(&std::fs::read(cache).expect("cache")).unwrap();
        let dir = std::env::temp_dir().join("kob-export");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for b in &set.books {
            let p = dir.join(highlights::file_name(&b.title));
            let first = obsidian::render(b, &obsidian::Extra::default());
            assert!(matches!(obsidian::merge(&first, b, &obsidian::Extra::default()), obsidian::Merge::Unchanged), "{}", b.title);
            let ids = first.lines().filter(|l| l.starts_with("^kob-")).count();
            let unique: std::collections::HashSet<_> = b.notes.iter().map(obsidian::block_id).collect();
            assert_eq!(ids, unique.len(), "{}", b.title);
            std::fs::write(p, first).unwrap();
        }
        println!("{} notes, {} highlights in {}", set.books.len(), set.books.iter().map(|b| b.notes.len()).sum::<usize>(), dir.display());
    }

    /// Claude Code and the highlights cache: `cargo test claude_anki -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn claude_anki() {
        tauri::async_runtime::block_on(async {
            let cache = home().join("Library/Caches/com.danisnowman.kobridge/highlights.json");
            let set: HighlightSet = serde_json::from_slice(&std::fs::read(cache).expect("cache")).unwrap();
            let b = set.books.iter().filter(|b| b.notes.len() >= 3).min_by_key(|b| b.notes.len()).expect("a book with 3+ highlights");
            let cwd = std::env::temp_dir().join("kob-anki");
            std::fs::create_dir_all(&cwd).unwrap();
            let t = std::time::Instant::now();
            let cards = anki_one(&obsidian::find_claude(&home()).expect("claude"), &cwd, b, "haiku", &Enrich::default()).await.expect("cards");
            let tsv = anki::tsv(&[(b, cards)]);
            println!("{} ({} subrayados), {:.1} s\n{tsv}", b.title, b.notes.len(), t.elapsed().as_secs_f64());
            let rows: Vec<_> = tsv.lines().filter(|l| !l.starts_with('#')).collect();
            assert!(!rows.is_empty());
            assert!(rows.iter().all(|r| r.split('\t').count() == 5 && r.starts_with("kob-")));
        });
    }

    /// Needs Claude Code, not the Kindle: `cargo test claude_enrich -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn claude_enrich() {
        tauri::async_runtime::block_on(async {
            let v = std::env::temp_dir().join("kob-vault");
            let _ = std::fs::remove_dir_all(&v);
            std::fs::create_dir_all(v.join(".obsidian")).unwrap();
            std::fs::create_dir_all(v.join("KOReader")).unwrap();
            std::fs::write(v.join("La gracia.md"), "# La gracia\n\nNotas sobre la gracia y el perdón en la teología cristiana.\n").unwrap();
            let n = |text: &str, dt: &str| highlights::Note { text: text.into(), note: None, chapter: Some("Capítulo 1".into()), page: Some(3), datetime: Some(dt.into()) };
            let b = highlights::Book { sidecar: String::new(), title: "Libro".into(), authors: "Autora".into(), percent: 0.3, md5: None, notes: vec![
                n("La gracia no se gana; se recibe como un regalo.", "2026-01-01 10:00:00"),
                n("El perdón cambia primero al que perdona.", "2026-01-02 10:00:00"),
            ]};
            let p = v.join("KOReader/Libro.md");
            std::fs::write(&p, obsidian::render(&b, &obsidian::Extra::default())).unwrap();
            let quotes = |d: &str| d.lines().filter(|l| l.starts_with('>') || l.starts_with("^kob-") || l.starts_with("## Cap")).map(str::to_string).collect::<Vec<_>>();
            let before = quotes(&std::fs::read_to_string(&p).unwrap());
            let claude = obsidian::find_claude(&home()).expect("claude");
            let t = std::time::Instant::now();
            enrich_one(&claude, &v, &p, "haiku", &Enrich::default(), |t| println!("  {t}")).await.expect("enrich");
            let after = std::fs::read_to_string(&p).unwrap();
            println!("{:.1} s\n{after}", t.elapsed().as_secs_f64());
            assert!(after.contains("## Resumen"));
            assert_eq!(quotes(&after), before, "quotes and block ids untouched");
            assert!(after.find("## Resumen") < after.find("## Capítulo 1"));
        });
    }

    /// Files view against `/mnt/us/kob test/`: `cargo test kindle_files -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn kindle_files() {
        use files::*;
        tauri::async_runtime::block_on(async {
            let root = "/mnt/us/kob test";
            let clean = format!("rm -rf {}", sq(root));
            probe().await.expect("status");
            ssh::run(&clean).await.unwrap();
            let states = |c: &Change| c.results.iter().map(|o| o.state.as_str()).collect::<Vec<_>>().join(" ");
            assert_eq!(states(&fs_mkdir(root.into()).await.unwrap()), "OK");
            assert_eq!(states(&fs_mkdir(root.into()).await.unwrap()), "EXISTS");
            let a = format!("{root}/a");
            fs_mkdir(a.clone()).await.unwrap();

            // upload a file and a folder (names with spaces, parens, a quote and an accent)
            let dir = std::env::temp_dir().join("kob-live-files");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("Carpeta (1) d'Ana/sub")).unwrap();
            std::fs::write(dir.join("Carpeta (1) d'Ana/sub/ñ.txt"), b"x").unwrap();
            // over 100 bytes: tar needs a pax or GNU long-name header that BusyBox must read
            let long = format!("{}.txt", "n".repeat(116));
            std::fs::write(dir.join("Carpeta (1) d'Ana/sub").join(&long), b"y").unwrap();
            std::fs::write(dir.join("Libro (1).epub"), b"hola").unwrap();
            let up = |n: &str| fs_upload(dir.join(n).to_string_lossy().into(), root.into());
            assert_eq!(states(&up("Libro (1).epub").await.unwrap()), "OK");
            assert_eq!(states(&up("Libro (1).epub").await.unwrap()), "EXISTS");
            let c = up("Carpeta (1) d'Ana").await.unwrap();
            assert!(c.dirs[root].iter().any(|e| e.name == "Carpeta (1) d'Ana" && e.kind == "dir"));
            let sub = fs_list(format!("{root}/Carpeta (1) d'Ana/sub")).await.unwrap();
            let mut names: Vec<_> = sub.iter().map(|e| e.name.clone()).collect();
            names.sort();
            assert_eq!(names, [long.clone(), "ñ.txt".into()]);

            // rename carries the sidecar; a sidecar shared with another format stays
            let book = format!("{root}/Libro (1).epub");
            ssh::run(&format!("mkdir {s} && echo 1 > {s}/metadata.epub.lua", s = sq(&format!("{root}/Libro (1).sdr")))).await.unwrap();
            let b = format!("{root}/B.epub");
            assert_eq!(states(&fs_rename(book.clone(), b.clone()).await.unwrap()), "OK");
            assert!(fs_list(root.into()).await.unwrap().iter().any(|e| e.name == "B.sdr"));
            ssh::run(&format!("echo 1 > {}", sq(&format!("{root}/B.sdr/metadata.pdf.lua")))).await.unwrap();
            let c = fs_rename(b.clone(), format!("{root}/C.epub")).await.unwrap();
            assert_eq!(states(&c), "OK KEPT");
            assert!(c.dirs[root].iter().any(|e| e.name == "B.sdr"));
            ssh::run(&format!("rm {}", sq(&format!("{root}/B.sdr/metadata.pdf.lua")))).await.unwrap();
            fs_rename(format!("{root}/B.sdr"), format!("{root}/C.sdr")).await.unwrap();

            // move both the book and its selected sidecar: the .sdr goes once, with the book
            let c = fs_move(vec![format!("{root}/C.epub"), format!("{root}/C.sdr")], a.clone()).await.unwrap();
            assert_eq!(states(&c), "OK");
            let names: Vec<_> = c.dirs[a.as_str()].iter().map(|e| e.name.clone()).collect();
            assert!(names.contains(&"C.epub".into()) && names.contains(&"C.sdr".into()), "{names:?}");

            // never overwrite
            std::fs::write(dir.join("C.epub"), b"otro").unwrap();
            up("C.epub").await.unwrap();
            assert_eq!(states(&fs_move(vec![format!("{root}/C.epub")], a.clone()).await.unwrap()), "EXISTS");
            assert_eq!(ssh::run(&format!("cat {}", sq(&format!("{a}/C.epub")))).await.unwrap(), "hola");

            // download a file and a folder
            let dest = dir.join("down.epub");
            assert_eq!(fs_download(format!("{a}/C.epub"), dest.to_string_lossy().into()).await.unwrap(), 4);
            let tar = dir.join("down.tar");
            fs_download(format!("{root}/Carpeta (1) d'Ana"), tar.to_string_lossy().into()).await.unwrap();
            let listing = std::process::Command::new("tar").arg("-tf").arg(&tar).output().unwrap();
            assert!(String::from_utf8_lossy(&listing.stdout).contains("sub/ñ.txt"));

            // delete purges only that format's metadata, then the empty sidecar
            let c = fs_delete(vec![format!("{a}/C.epub")]).await.unwrap();
            assert_eq!(states(&c), "OK");
            assert!(c.dirs[a.as_str()].is_empty(), "{:?}", c.dirs[a.as_str()]);

            // guards
            assert!(fs_delete(vec!["/mnt/us/documents".into()]).await.is_err());
            assert!(fs_rename("/mnt/us/koreader/reader.lua".into(), "/mnt/us/koreader/r.lua".into()).await.is_err());
            assert!(fs_mkdir(format!("{root}/../x")).await.is_err());

            let c = fs_delete(vec![root.into()]).await.unwrap();
            assert_eq!(states(&c), "OK");
            assert!(!c.dirs["/mnt/us"].iter().any(|e| e.name == "kob test"));
            ssh::run(&clean).await.unwrap();
            std::fs::remove_dir_all(&dir).ok();
            println!("files ok");
        });
    }

    #[test]
    #[ignore]
    fn kindle_size() {
        tauri::async_runtime::block_on(async {
            let s = backup_size().await.expect("size");
            let mb = |b: u64| b / 1024 / 1024;
            println!("books {} MB in {} files · base {} MB · full {} MB · {:.2} MB/s · full ≈ {:.0} min",
                mb(s.books_bytes), s.books_files, mb(s.base_bytes), mb(s.full_bytes), s.bytes_per_sec / 1048576.0,
                s.full_bytes as f64 / s.bytes_per_sec / 60.0);
            assert!(s.full_bytes > s.books_bytes && s.base_bytes > 0);
        });
    }
}

