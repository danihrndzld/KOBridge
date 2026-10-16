<script lang="ts">
  import { onMount, tick } from 'svelte'
  import { getCurrentWebview } from '@tauri-apps/api/webview'
  import { Menu } from '@tauri-apps/api/menu'
  import { open as pick, save, ask } from '@tauri-apps/plugin-dialog'
  import Loading from '../Loading.svelte'
  import Cable from '../Cable.svelte'
  import { api, errText, fmtBytes, type FsEntry } from '../api'
  import { app, refreshStatus, saveSettings } from '../store.svelte'
  import { t, dec, i18n } from '../i18n.svelte'
  import { fs, dirs, US, load, go, back, up, snapshot, change, canChange, canWriteIn, entryOf, join, parentOf, baseOf } from '../files.svelte'

  const FOLDER = 'M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z'
  const FILE = 'M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7ZM14 2v4a2 2 0 0 0 2 2h4'
  const mac = navigator.userAgent.includes('Mac')
  const mod = (e: { metaKey: boolean; ctrlKey: boolean }) => (mac ? e.metaKey : e.ctrlKey)
  const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })
  const df = $derived(new Intl.DateTimeFormat(i18n.lang, { dateStyle: 'medium', timeStyle: 'short' }))

  let root: HTMLDivElement
  let listEl = $state<HTMLDivElement>()
  const online = $derived(!!app.status)
  const isSdr = (e: FsEntry) => e.kind === 'dir' && e.name.endsWith('.sdr')
  const rows = $derived.by(() => {
    const show = app.settings?.show_sdr ?? false
    const list = (dirs.get(fs.cwd) ?? []).filter((e) => show || !isSdr(e))
    const { key, asc } = fs.sort
    const by = (a: FsEntry, b: FsEntry) => (key === 'name' ? 0 : a[key] - b[key]) || collator.compare(a.name, b.name)
    return [...list].sort((a, b) => Number(b.kind === 'dir') - Number(a.kind === 'dir') || (asc ? by(a, b) : by(b, a)))
  })
  const paths = $derived(rows.map((r) => join(fs.cwd, r.name)))
  const sel = $derived(new Set(fs.selected))
  const pend = $derived(new Set(fs.pending))
  let last: Menu | undefined
  const crumbs = $derived(fs.cwd.slice(US.length).split('/').filter(Boolean)
    .reduce((a, n) => [...a, { name: n, path: join(a[a.length - 1].path, n) }], [{ name: 'Kindle', path: US }]))
  const writable = $derived(online && canWriteIn(fs.cwd))

  // ---- selection and keys ----
  let cursor = ''
  function pickRow(p: string, add: boolean, range: boolean) {
    if (range && fs.anchor) {
      const [a, b] = [paths.indexOf(fs.anchor), paths.indexOf(p)].sort((x, y) => x - y)
      fs.selected = a < 0 ? [p] : paths.slice(a, b + 1)
    } else if (add) {
      fs.selected = sel.has(p) ? fs.selected.filter((x) => x !== p) : [...fs.selected, p]
      fs.anchor = p
    } else {
      fs.selected = [p]
      fs.anchor = p
    }
    cursor = p
  }
  const reveal = (p: string) => root.querySelector(`[data-path="${CSS.escape(p)}"]`)?.scrollIntoView({ block: 'nearest' })

  let typed = ''
  let typedAt = 0
  function onKey(e: KeyboardEvent) {
    // only keys meant for the list; a focused button keeps its Space and Enter
    if (!root?.checkVisibility() || renaming || !(e.target === document.body || listEl?.contains(e.target as Node))
      || (e.target as HTMLElement).closest('input, textarea, select')) return
    const m = mod(e)
    const one = fs.selected.length === 1 ? fs.selected[0] : ''
    if (e.key === 'ArrowDown' && m) { if (one && entryOf(one)?.kind === 'dir') go(one) }
    else if (e.key === 'ArrowUp' && m) up()
    else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      if (!paths.length) return
      const i = paths.indexOf(cursor)
      const n = paths[i < 0 ? (e.key === 'ArrowDown' ? 0 : paths.length - 1) : Math.max(0, Math.min(paths.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))]
      pickRow(n, false, e.shiftKey)
      reveal(n)
    }
    else if (e.key === 'Enter') { if (one && online && canChange(one)) startRename(one) }
    else if (e.key === 'Backspace' && m) remove()
    else if (m && e.shiftKey && e.key.toLowerCase() === 'n') newFolder()
    else if (e.key.length === 1 && !m && !e.altKey && !e.ctrlKey) {
      if (performance.now() - typedAt > 800) typed = ''
      typed += e.key.toLowerCase()
      typedAt = performance.now()
      const hit = rows.find((r) => r.name.toLowerCase().startsWith(typed))
      if (hit) { pickRow(join(fs.cwd, hit.name), false, false); reveal(join(fs.cwd, hit.name)) }
    }
    else return
    e.preventDefault()
  }

  // ---- internal move: pointer events, since HTML5 drag is off while Tauri's native drop handler is on ----
  let drag = $state<{ paths: string[]; p: string; x: number; y: number; on: boolean; collapse: boolean } | null>(null)
  let at = $state({ x: 0, y: 0 })
  let target = $state('')
  /** The folder row or path segment under the point, if it can take these items. */
  function folderAt(x: number, y: number) {
    return (document.elementFromPoint(x, y) as HTMLElement | null)?.closest<HTMLElement>('[data-drop]')?.dataset.drop ?? ''
  }
  function down(e: PointerEvent, p: string) {
    if (e.button !== 0 || renaming === p) return
    listEl?.focus()
    const was = sel.has(p)
    if (!was || mod(e) || e.shiftKey) pickRow(p, mod(e), e.shiftKey)
    if (!sel.has(p)) return
    drag = { paths: [...fs.selected], p, x: e.clientX, y: e.clientY, on: false, collapse: was && !mod(e) && !e.shiftKey }
    ;(e.currentTarget as Element).setPointerCapture(e.pointerId)
  }
  function moving(e: PointerEvent) {
    if (!drag || (!drag.on && Math.hypot(e.clientX - drag.x, e.clientY - drag.y) < 4)) return
    drag.on = true
    at = { x: e.clientX, y: e.clientY }
    const d = folderAt(e.clientX, e.clientY)
    const ok = online && drag.paths.every(canChange) && canWriteIn(d)
      && !drag.paths.some((p) => d === p || d.startsWith(`${p}/`) || parentOf(p) === d)
    target = ok ? d : ''
  }
  function dropped() {
    if (!drag) return
    const d = drag
    drag = null
    if (d.on && target) { const dir = target; change(d.paths, () => api.fsMove(d.paths, dir)) }
    else if (!d.on && d.collapse) pickRow(d.p, false, false)
    target = ''
  }
  const cancelDrag = () => { drag = null; target = '' }

  // ---- uploads from Finder, one at a time ----
  type Up = { path: string; dir: string }
  let queue = $state<Up[]>([])
  let sending = $state<Up | null>(null)
  let sent = $state(0)
  let dropDir = $state('')
  function enqueue(list: string[], dir: string) {
    if (!sending) { sent = 0; fs.notes = [] }
    queue.push(...list.map((path) => ({ path, dir })))
    pump()
  }
  async function pump() {
    if (sending) return
    for (let j = queue.shift(); j; j = queue.shift()) {
      sending = j
      const c = await change([], () => api.fsUpload(j.path, j.dir), true)
      if (c?.results[0]?.state === 'OK') sent++
      else if (!c) load(j.dir)
    }
    sending = null
    if (sent) refreshStatus()
  }
  async function chooseUpload() {
    const picked = await pick({ multiple: true })
    if (picked) enqueue(Array.isArray(picked) ? picked : [picked], fs.cwd)
  }

  onMount(() => {
    snapshot(false)
    const un = getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload
      if (!root?.checkVisibility() || p.type === 'leave') { dropDir = ''; return }
      // macOS reports points, the same unit elementFromPoint takes
      const d = folderAt(p.position.x, p.position.y) || fs.cwd
      dropDir = online && canWriteIn(d) ? d : ''
      if (p.type === 'drop') {
        if (dropDir) enqueue(p.paths, dropDir)
        dropDir = ''
      }
    })
    return () => { un.then((f) => f()) }
  })

  // list the folder and warm up the map once per connection
  let wasOnline = false
  $effect(() => {
    const on = !!app.status
    if (on && !wasOnline) { load(fs.cwd); snapshot(true) }
    wasOnline = on
  })

  // ---- actions ----
  let renaming = $state('')
  let draft = $state('')
  async function startRename(p: string) {
    renaming = p
    draft = baseOf(p)
    await tick()
    const input = root.querySelector<HTMLInputElement>('input.rename')
    const dot = entryOf(p)?.kind === 'file' ? draft.lastIndexOf('.') : -1
    input?.focus()
    input?.setSelectionRange(0, dot > 0 ? dot : draft.length)
  }
  async function commitRename() {
    const p = renaming
    const name = draft.trim()
    renaming = ''
    if (!p || !name || name === baseOf(p)) return
    if (name.includes('/') || name.startsWith('.')) {
      fs.notes = [{ kind: 'bad', text: t('A name can\'t contain "/" or start with ".".', 'Un nombre no puede tener "/" ni empezar con ".".') }]
      return
    }
    const to = join(parentOf(p), name)
    const c = await change([p], () => api.fsRename(p, to))
    if (c?.results[0]?.state === 'OK') { fs.selected = [to]; fs.anchor = cursor = to }
  }

  let creating = $state(false)
  async function newFolder() {
    if (!writable || creating) return
    const names = new Set((dirs.get(fs.cwd) ?? []).map((e) => e.name))
    const stem = t('untitled folder', 'carpeta sin título')
    let name = stem
    for (let n = 2; names.has(name); n++) name = `${stem} ${n}`
    const p = join(fs.cwd, name)
    creating = true
    const c = await change([], () => api.fsMkdir(p))
    creating = false
    if (c?.results[0]?.state === 'OK') { pickRow(p, false, false); startRename(p) }
  }

  async function remove() {
    const list = [...fs.selected]
    if (!list.length || !online || !list.every(canChange)) return
    const what = list.length === 1 ? `"${baseOf(list[0])}"` : `${list.length} ${t('items', 'elementos')}`
    const ok = await ask(t(`Delete ${what} from the Kindle? This can't be undone.`, `¿Borrar ${what} de la Kindle? No se puede deshacer.`),
      { title: t('Delete', 'Borrar'), kind: 'warning', okLabel: t('Delete', 'Borrar'), cancelLabel: t('Cancel', 'Cancelar') })
    if (ok) await change(list, () => api.fsDelete(list))
  }

  let downloading = $state('')
  async function download(p: string) {
    const dir = entryOf(p)?.kind === 'dir'
    const dest = await save({ defaultPath: dir ? `${baseOf(p)}.tar` : baseOf(p) })
    if (!dest) return
    downloading = baseOf(p)
    fs.notes = []
    try {
      await api.fsDownload(p, dest)
      fs.notes = [{ kind: 'ok', text: t(`Saved ${dest}`, `Guardé ${dest}`), reveal: dest }]
    } catch (e) {
      fs.notes = [{ kind: 'bad', text: errText(e) }]
    } finally {
      downloading = ''
    }
  }

  let refreshing = $state(false)
  async function refresh() {
    refreshing = true
    await load(fs.cwd)
    refreshing = false
  }

  async function menu(e: MouseEvent, p?: string) {
    e.preventDefault()
    if (!p) fs.selected = []
    else if (!sel.has(p)) pickRow(p, false, false)
    const list = [...fs.selected]
    const one = list.length === 1 ? entryOf(list[0]) : undefined
    const free = online && list.length > 0 && list.every(canChange)
    const m: Menu = await Menu.new({ items: [
      { text: t('Open', 'Abrir'), enabled: one?.kind === 'dir', action: () => go(list[0]) },
      { text: t('Rename', 'Renombrar'), enabled: !!one && free, action: () => startRename(list[0]) },
      { text: t('Download to…', 'Descargar en…'), enabled: !!one && online, action: () => download(list[0]) },
      { item: 'Separator' },
      { text: t('New Folder', 'Nueva carpeta'), enabled: writable, action: newFolder },
      { item: 'Separator' },
      { text: t('Delete', 'Borrar'), enabled: free, action: remove },
    ] })
    // the previous menu is closed here, not after popup(): on GTK popup() resolves before the menu is dismissed
    await last?.close()
    last = m
    await m.popup()
  }

  const sortBy = (key: typeof fs.sort.key) => (fs.sort = { key, asc: fs.sort.key === key ? !fs.sort.asc : key === 'name' })
  const arrow = (key: typeof fs.sort.key) => (fs.sort.key === key ? (fs.sort.asc ? ' ↑' : ' ↓') : '')
  const gb = (kb: number) => dec(kb / 1024 / 1024)
</script>

<svelte:window onkeydown={onKey} />

<div class="stack" bind:this={root}>
  <div class="head">
    <div>
      <h1>{t('Files', 'Archivos')}</h1>
      <p class="sub num">{rows.length} {t('items', 'elementos')}{#if app.status}{' · '}{gb(app.status.free_kb)} {t('GB free', 'GB libres')}{/if}</p>
    </div>
    <div class="row">
      <button class="btn ghost icon" disabled={!fs.history.length} onclick={back} aria-label={t('Back', 'Atrás')} title={t('Back', 'Atrás')}>
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m15 18-6-6 6-6" /></svg>
      </button>
      <button class="btn ghost" disabled={!writable || creating} onclick={newFolder}>{creating ? t('Creating…', 'Creando…') : t('New folder', 'Nueva carpeta')}</button>
      <button class="btn ghost" disabled={!writable} onclick={chooseUpload}>{t('Upload…', 'Subir…')}</button>
      <button class="btn ghost" disabled={!online || refreshing} onclick={refresh}>{refreshing ? t('Refreshing…', 'Actualizando…') : t('Refresh', 'Actualizar')}</button>
    </div>
  </div>

  <Cable />

  <div class="bar">
    <nav class="crumbs" aria-label={t('Path', 'Ruta')}>
      {#each crumbs as c, i (c.path)}
        {#if i}<span aria-hidden="true">/</span>{/if}
        <button data-drop={c.path} class:target={target === c.path || dropDir === c.path} aria-current={c.path === fs.cwd ? 'page' : undefined} onclick={() => go(c.path)}>{c.name}</button>
      {/each}
    </nav>
    <label class="row sdr-tog">
      <input type="checkbox" class="tog" checked={app.settings?.show_sdr ?? false} onchange={(e) => saveSettings({ show_sdr: e.currentTarget.checked })} />
      {t('Show KOReader data folders', 'Mostrar carpetas de datos de KOReader')}
    </label>
  </div>

  {#if sending}
    <div class="strip num" role="status"><span class="spin" aria-hidden="true"></span>{t('Uploading', 'Subiendo')} {baseOf(sending.path)} · {sent + 1} {t('of', 'de')} {sent + 1 + queue.length}</div>
  {:else if downloading}
    <div class="strip" role="status"><span class="spin" aria-hidden="true"></span>{t('Downloading', 'Descargando')} {downloading}…</div>
  {/if}
  {#if fs.error}<div class="banner bad">{fs.error}</div>{/if}
  {#each fs.notes as n, i (i)}
    <div class="banner {n.kind}">
      <span class="selectable">{n.text}</span>
      {#if n.reveal}{@const r = n.reveal}<button class="link" onclick={() => api.reveal(r)}>{mac ? t('Show in Finder', 'Mostrar en Finder') : t('Show in folder', 'Mostrar en la carpeta')}</button>{/if}
    </div>
  {/each}

  {#if fs.loading === fs.cwd}
    <Loading title={t('Reading the folder…', 'Leyendo la carpeta…')} detail={fs.cwd} />
  {:else if !dirs.has(fs.cwd) && !online}
    <div class="empty">{t('Kindle offline. Connect it to browse its files.', 'Kindle sin conexión. Conectala para ver sus archivos.')}</div>
  {:else}
    <div class="table">
      <div class="cols">
        <span></span>
        <button onclick={() => sortBy('name')}>{t('Name', 'Nombre')}{arrow('name')}</button>
        <button class="r-al" onclick={() => sortBy('size')}>{t('Size', 'Tamaño')}{arrow('size')}</button>
        <button onclick={() => sortBy('mtime')}>{t('Modified', 'Modificado')}{arrow('mtime')}</button>
      </div>
      <div class="rows num" class:over={dropDir === fs.cwd} role="listbox" tabindex="0" aria-multiselectable="true" aria-label={fs.cwd} bind:this={listEl}
        oncontextmenu={(e) => menu(e)} onpointerdown={(e) => { if (e.target === e.currentTarget) { fs.selected = []; listEl?.focus() } }}>
        {#each rows as r (r.name)}
          {@const p = join(fs.cwd, r.name)}
          <div class="r" role="option" tabindex="-1" aria-selected={sel.has(p)} data-path={p} data-drop={r.kind === 'dir' ? p : undefined}
            class:target={target === p || dropDir === p} class:pending={pend.has(p)} class:dim={isSdr(r)}
            onpointerdown={(e) => down(e, p)} onpointermove={moving} onpointerup={dropped} onpointercancel={cancelDrag}
            ondblclick={() => { if (r.kind === 'dir') go(p) }} oncontextmenu={(e) => { e.stopPropagation(); menu(e, p) }}>
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d={r.kind === 'dir' ? FOLDER : FILE} /></svg>
            {#if renaming === p}
              <input class="rename" bind:value={draft} onblur={commitRename} aria-label={t('New name', 'Nombre nuevo')}
                onkeydown={(e) => { e.stopPropagation(); if (e.key === 'Enter') commitRename(); else if (e.key === 'Escape') renaming = '' }} />
            {:else}
              <span class="ellipsis" title={r.name}>{r.name}</span>
            {/if}
            <span class="meta r-al">{r.kind === 'dir' ? '—' : fmtBytes(r.size)}</span>
            <span class="meta ellipsis">{df.format(r.mtime * 1000)}</span>
          </div>
        {:else}
          <p class="none">{t('This folder is empty.', 'Esta carpeta está vacía.')}</p>
        {/each}
      </div>
    </div>
  {/if}
</div>

{#if drag?.on}
  <div class="ghost num" style:left="{at.x + 14}px" style:top="{at.y + 10}px">
    {drag.paths.length === 1 ? baseOf(drag.paths[0]) : `${drag.paths.length} ${t('items', 'elementos')}`}
  </div>
{/if}

<style>
  .icon { padding: 7px 9px }
  .icon svg { width: 15px; height: 15px; display: block; stroke: currentColor; fill: none; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round }
  .bar { display: flex; justify-content: space-between; align-items: center; gap: 12px; flex-wrap: wrap }
  .crumbs { display: flex; flex-wrap: wrap; align-items: center; gap: 2px; color: var(--ink-2); min-width: 0 }
  .crumbs button { appearance: none; border: 0; background: none; font: inherit; color: inherit; padding: 3px 6px; border-radius: 5px }
  .crumbs button:hover, .crumbs button.target { background: var(--rule); color: var(--ink) }
  .crumbs button[aria-current='page'] { color: var(--ink); font-weight: 600 }
  .sdr-tog { font-size: 12.5px; color: var(--ink-2) }
  .strip { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--ink-2) }
  .spin { width: 12px; height: 12px; flex: none; border-radius: 50%; border: 2px solid var(--rule); border-top-color: var(--ink); animation: r 0.8s linear infinite }
  @keyframes r { to { transform: rotate(360deg) } }
  .table { border: 1px solid var(--rule); border-radius: 8px; overflow: hidden }
  .cols, .r { display: grid; grid-template-columns: 16px minmax(0, 1fr) 76px 168px; gap: 10px; align-items: center; padding: 0 12px }
  .cols { height: 30px; border-bottom: 1px solid var(--rule); background: var(--paper) }
  .cols button { appearance: none; border: 0; background: none; padding: 0; font: 500 11.5px var(--sans); color: var(--ink-2); text-align: left }
  .cols button:hover { color: var(--ink) }
  .r-al { text-align: right !important }
  .rows { min-height: 220px; outline: none; transition: background .15s }
  .rows.over { background: var(--paper) }
  .r { height: 28px; content-visibility: auto; contain-intrinsic-size: auto 28px; outline: none }
  .r svg { width: 15px; height: 15px; stroke: currentColor; fill: none; stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round }
  .r[aria-selected='true'] { background: var(--rule) }
  .rows:focus-within .r[aria-selected='true'] { background: var(--ink); color: var(--paper) }
  .rows:focus-within .r[aria-selected='true'] .meta { color: inherit }
  .r.target, .rows:focus-within .r.target { background: var(--ink-2); color: var(--paper) }
  .r.pending { opacity: .45 }
  .r.dim:not([aria-selected='true']) { color: var(--ink-2) }
  .meta { color: var(--ink-2); font-size: 12px }
  .rename { font: inherit; min-width: 0; padding: 2px 6px; border: 1px solid var(--ink-2); border-radius: 4px; background: var(--sheet); color: var(--ink); outline: none; user-select: text }
  .none { color: var(--ink-2); text-align: center; margin: 0; padding: 28px }
  .ghost { position: fixed; z-index: 10; pointer-events: none; max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; padding: 3px 9px; border-radius: 99px; background: var(--ink); color: var(--paper); font-size: 11.5px }
  @media (prefers-reduced-motion: reduce) { .spin { animation: none } }
</style>
