import { invoke, Channel } from '@tauri-apps/api/core'
import { i18n, dec, type Lang } from './i18n.svelte'

export const KO = '/mnt/us/koreader'

export type Status = { host: string; battery: number | null; free_kb: number; total_kb: number; reader_running: boolean }
export type Settings = { backup_dir: string; vault_dir: string; backup_books: boolean; auto_backup: boolean; last_auto_backup: string; ai_model: Model; lang: Lang; show_sdr: boolean }
export type BackupFile = { name: string; path: string; size: number; modified: number }
export type BackupSize = { books_bytes: number; books_files: number; base_bytes: number; full_bytes: number; bytes_per_sec: number }
export type Day = { date: string; minutes: number }
export type StatBook = { title: string; authors: string; pages: number; percent: number; hours: number; last_open: number }
export type Stats = { total_hours: number; days_read: number; books: number; avg_minutes: number; snapshot_at: number; daily: Day[]; recent: StatBook[] }
export type Note = { text: string; note: string | null; chapter: string | null; page: number | null; datetime: string | null }
export type HBook = { sidecar: string; title: string; authors: string; percent: number; md5?: string | null; notes: Note[] }
export type ExportReport = { created: number; updated: number; added: number; legacy: number; dir: string }
export type Model = 'haiku' | 'sonnet' | 'opus' | 'fable'
export type EnrichEvent = { book: string; kind: 'start' | 'tool' | 'done' | 'error'; text: string }
export type HighlightSet = { books: HBook[]; errors: string[]; fetched_at: number }
export type Entry = { name: string; size: number }
export type Kind = 'books' | 'fonts' | 'patches'
export type StoreKind = 'plugin' | 'patch' | 'font'
export type StoreItem = { id: string; name: string; owner: string; description: string; stars: number; pushed_at: string; url: string; version?: string; repo?: string; category?: string }
export type Catalog = { generated_at: string; offline: string | null; plugins: StoreItem[]; patches: StoreItem[]; fonts: StoreItem[] }
export type Owned = { kind: StoreKind; id: string; version: string; update: boolean; removable: boolean }
export type StoreState = { items: Owned[]; registry_error: string | null }
export type StoreStep = { step: 'backup' | 'download' | 'send' | 'done'; done: number; total: number }
export type FsEntry = { name: string; kind: 'dir' | 'file' | 'link'; size: number; mtime: number }
export type FsOutcome = { state: 'OK' | 'EXISTS' | 'ERR' | 'KEPT'; path: string }
export type FsChange = { results: FsOutcome[]; dirs: Record<string, FsEntry[]> }
export type SshInfo = { key_path: string; public_key: string | null; config_path: string; has_host: boolean; hostname: string; port: string; user: string; identity: string; usb: string | null }

export const api = {
  status: () => invoke<Status>('status'),
  getSettings: () => invoke<Settings>('get_settings'),
  setSettings: (settings: Settings) => invoke<void>('set_settings', { settings }),
  backupRun: (stamp: string, onBytes: (n: number) => void) => {
    const progress = new Channel<number>()
    progress.onmessage = onBytes
    return invoke<BackupFile>('backup_run', { stamp, progress })
  },
  backupSize: () => invoke<BackupSize>('backup_size'),
  backupList: () => invoke<BackupFile[]>('backup_list'),
  reveal: (path: string) => invoke<void>('reveal', { path }),
  openUrl: (url: string) => invoke<void>('open_url', { url }),
  stats: (refresh: boolean) => invoke<Stats | null>('stats', { refresh }),
  highlights: (refresh: boolean) => invoke<HighlightSet>('highlights', { refresh }),
  highlightsExport: (books: HBook[]) => invoke<ExportReport>('highlights_export', { books }),
  obsidianVaults: () => invoke<string[]>('obsidian_vaults'),
  obsidianCreateVault: () => invoke<string>('obsidian_create_vault'),
  enrichStart: (books: HBook[], model: Model, today: string, onEvent: (e: EnrichEvent) => void) => {
    const events = new Channel<EnrichEvent>()
    events.onmessage = onEvent
    return invoke<void>('enrich_start', { books, model, today, events })
  },
  ankiStart: (books: HBook[], model: Model, today: string, onEvent: (e: EnrichEvent) => void) => {
    const events = new Channel<EnrichEvent>()
    events.onmessage = onEvent
    return invoke<string | null>('anki_start', { books, model, today, events })
  },
  ankiOpen: (path: string) => invoke<void>('anki_open', { path }),
  enrichStop: () => invoke<void>('enrich_stop'),
  sendFile: (path: string, kind: Kind) => invoke<string>('send_file', { path, kind }),
  listDir: (kind: Kind) => invoke<Entry[]>('list_dir', { kind }),
  readRemote: (path: string) => invoke<string>('read_remote', { path }),
  writeRemote: (path: string, content: string) => invoke<void>('write_remote', { path, content }),
  renameRemote: (from: string, to: string) => invoke<void>('rename_remote', { from, to }),
  deleteRemote: (path: string) => invoke<void>('delete_remote', { path }),
  logStart: (onLine: (l: string) => void) => {
    const lines = new Channel<string>()
    lines.onmessage = onLine
    return invoke<void>('log_start', { lines })
  },
  logStop: () => invoke<void>('log_stop'),
  screenMessage: (text: string) => invoke<void>('screen_message', { text }),
  sshSetupInfo: () => invoke<SshInfo>('ssh_setup_info'),
  sshCreateKey: () => invoke<void>('ssh_create_key'),
  sshAddHost: (ip: string, port: number) => invoke<void>('ssh_add_host', { ip, port }),
  sshInstallUsb: () => invoke<string>('ssh_install_usb'),
  sshTest: () => invoke<string>('ssh_test'),
  openSshConfig: () => invoke<void>('open_ssh_config'),
  storeCatalog: (refresh: boolean) => invoke<Catalog>('store_catalog', { refresh }),
  storeInstalled: () => invoke<StoreState>('store_installed'),
  storeRun: (op: 'install' | 'uninstall', kind: StoreKind, id: string, stamp: string, onStep: (s: StoreStep) => void) => {
    const progress = new Channel<StoreStep>()
    progress.onmessage = onStep
    return invoke<void>(`store_${op}`, { kind, id, stamp, progress })
  },
  fsList: (dir: string) => invoke<FsEntry[]>('fs_list', { dir }),
  fsSnapshot: (refresh: boolean) => invoke<Record<string, FsEntry[]>>('fs_snapshot', { refresh }),
  fsRename: (from: string, to: string) => invoke<FsChange>('fs_rename', { from, to }),
  fsMove: (paths: string[], dir: string) => invoke<FsChange>('fs_move', { paths, dir }),
  fsDelete: (paths: string[]) => invoke<FsChange>('fs_delete', { paths }),
  fsMkdir: (path: string) => invoke<FsChange>('fs_mkdir', { path }),
  fsUpload: (path: string, dir: string) => invoke<FsChange>('fs_upload', { path, dir }),
  fsDownload: (path: string, dest: string) => invoke<number>('fs_download', { path, dest }),
}

export const fmtBytes = (n: number) =>
  n < 1024 ? `${n} B` : n < 1024 ** 2 ? `${(n / 1024).toFixed(0)} KB` : n < 1024 ** 3 ? `${dec(n / 1024 ** 2)} MB` : `${dec(n / 1024 ** 3)} GB`

/** "6 oct" / "Oct 6" */
export const fmtDay = (d: Date) => d.toLocaleDateString(i18n.lang, { day: 'numeric', month: 'short' })

/** "today, 14:52" / "Oct 6, 11:53" from unix seconds */
export function fmtWhen(secs: number) {
  const d = new Date(secs * 1000)
  const today = new Date().toDateString() === d.toDateString()
  const time = d.toLocaleTimeString(i18n.lang, { hour: '2-digit', minute: '2-digit' })
  return `${today ? (i18n.lang === 'es' ? 'hoy' : 'today') : fmtDay(d)}, ${time}`
}

/** "6 min" / "1 h 12 min" / "40 s" */
export function fmtDuration(secs: number) {
  if (secs < 60) return `${Math.max(1, Math.round(secs))} s`
  const m = Math.round(secs / 60)
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${m % 60} min`
}

export const today = () => new Date().toLocaleDateString('sv') // YYYY-MM-DD

export const errText = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e))
