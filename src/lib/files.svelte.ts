import { SvelteMap } from 'svelte/reactivity'
import { api, errText, type FsChange, type FsEntry } from './api'
import { t } from './i18n.svelte'

export const US = '/mnt/us'
const PROTECTED = ['documents', 'audible', 'kpm', 'voice', 'libkh', 'mrpackages', 'system', 'extensions', 'koreader']
const KO_RW = ['patches', 'plugins', 'fonts', 'styletweaks', 'screenshots', 'data', 'settings']

export const join = (dir: string, name: string) => `${dir}/${name}`
export const parentOf = (p: string) => p.slice(0, p.lastIndexOf('/'))
export const baseOf = (p: string) => p.slice(p.lastIndexOf('/') + 1)

/** Mirrors `check` in files.rs (which also refuses koreader/settings while KOReader runs). */
export function canChange(p: string) {
  if (!p.startsWith(`${US}/`) || p.includes('\n')) return false
  const s = p.slice(US.length + 1).split('/')
  if (s.length === 1) return !PROTECTED.includes(s[0])
  return s[0] !== 'koreader' || (s.length > 2 && KO_RW.includes(s[1]))
}
export const canWriteIn = (dir: string) => canChange(join(dir, 'x'))

/** dir → entries. Arrays are replaced whole, never mutated, so rows stay cheap. */
export const dirs = new SvelteMap<string, FsEntry[]>()
// folders listed by the Kindle this session; the snapshot never replaces them
const live = new Set<string>()

export type Note = { kind: 'ok' | 'warn' | 'bad'; text: string; reveal?: string }

export const fs = $state({
  cwd: US,
  history: [] as string[],
  selected: [] as string[],
  anchor: '',
  sort: { key: 'name' as 'name' | 'size' | 'mtime', asc: true },
  pending: [] as string[],
  loading: '',
  error: '',
  notes: [] as Note[],
})

export const entryOf = (p: string) => dirs.get(parentOf(p))?.find((e) => e.name === baseOf(p))

function put(dir: string, list: FsEntry[]) {
  dirs.set(dir, list)
  live.add(dir)
}

export async function load(dir: string) {
  if (!dirs.has(dir)) fs.loading = dir
  try {
    put(dir, await api.fsList(dir))
    if (dir === fs.cwd) fs.error = ''
  } catch (e) {
    // a folder that no longer exists leaves the map; a connection error keeps the cached copy
    if (/doesn't exist anymore|ya no existe/.test(errText(e))) { dirs.delete(dir); live.delete(dir) }
    if (dir === fs.cwd) fs.error = errText(e)
  } finally {
    if (fs.loading === dir) fs.loading = ''
  }
}

export function go(dir: string, push = true) {
  if (dir === fs.cwd) return
  if (push) fs.history.push(fs.cwd)
  fs.cwd = dir
  fs.selected = []
  fs.anchor = ''
  fs.error = ''
  load(dir)
}
export const back = () => { const d = fs.history.pop(); if (d) go(d, false) }
export const up = () => { if (fs.cwd !== US) go(parentOf(fs.cwd)) }

/** `refresh: false` loads the cached copy; `true` reads the whole Kindle (~5 s). */
export async function snapshot(refresh: boolean) {
  try {
    for (const [d, list] of Object.entries(await api.fsSnapshot(refresh)))
      if (!live.has(d) && (refresh || !dirs.has(d))) dirs.set(d, list)
  } catch {
    // each folder still lists on open
  }
}

/** Runs one change with `paths` dimmed, then applies the folders the reply lists. */
export async function change(paths: string[], run: () => Promise<FsChange>, keepNotes = false) {
  fs.pending = [...fs.pending, ...paths]
  if (!keepNotes) fs.notes = []
  try {
    const c = await run()
    const done = new Set(c.results.filter((r) => r.state === 'OK').map((r) => r.path))
    // a renamed, moved or deleted folder's subfolders are refetched when opened
    const gone = [...done].filter((p) => dirs.has(p) || entryOf(p)?.kind === 'dir')
    if (gone.length) for (const k of [...dirs.keys()]) if (gone.some((p) => k === p || k.startsWith(`${p}/`))) { dirs.delete(k); live.delete(k) }
    for (const [d, list] of Object.entries(c.dirs)) put(d, list)
    fs.selected = fs.selected.filter((p) => !done.has(p))
    fs.notes = [...fs.notes, ...c.results.flatMap(describe)]
    return c
  } catch (e) {
    fs.notes = [...fs.notes, { kind: 'bad', text: errText(e) }]
  } finally {
    fs.pending = fs.pending.filter((p) => !paths.includes(p))
  }
}

function describe(r: FsChange['results'][number]): Note[] {
  const n = baseOf(r.path)
  if (r.state === 'EXISTS') return [{ kind: 'bad', text: t(`"${n}" already exists there, so nothing was replaced.`, `"${n}" ya existe ahí, así que no reemplacé nada.`) }]
  if (r.state === 'ERR') return [{ kind: 'bad', text: t(`Couldn't change "${n}".`, `No pude cambiar "${n}".`) }]
  if (r.state === 'KEPT') return [{ kind: 'warn', text: t(`KOReader data "${n}" stayed in place: another format of the book uses it, or one already exists there.`, `Los datos de KOReader "${n}" quedaron en su lugar: otro formato del libro los usa, o ya hay unos ahí.`) }]
  return []
}
