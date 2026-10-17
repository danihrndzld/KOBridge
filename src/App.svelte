<script lang="ts">
  import { onMount } from 'svelte'
  import { getCurrentWindow } from '@tauri-apps/api/window'
  import { app, refreshStatus, loadSettings, maybeAutoBackup, updater, checkUpdate, installUpdate, updateLabel } from './lib/store.svelte'
  import { t, dec } from './lib/i18n.svelte'
  import Backup from './lib/views/Backup.svelte'
  import Stats from './lib/views/Stats.svelte'
  import Highlights from './lib/views/Highlights.svelte'
  import Send from './lib/views/Send.svelte'
  import Patches from './lib/views/Patches.svelte'
  import Config from './lib/views/Config.svelte'
  import Log from './lib/views/Log.svelte'
  import Fonts from './lib/views/Fonts.svelte'
  import Prefs from './lib/views/Prefs.svelte'
  import Store from './lib/views/Store.svelte'
  import Files from './lib/views/Files.svelte'

  // Lucide paths, inlined
  const views = $derived([
    { id: 'backup', label: t('Backup', 'Respaldo'), icon: 'M21 8v13H3V8M1 3h22v5H1zM10 12h4', C: Backup },
    { id: 'stats', label: t('Reading', 'Lectura'), icon: 'M3 3v18h18M7 16v-4M12 16V8M17 16v-7', C: Stats },
    { id: 'hl', label: t('Highlights', 'Subrayados'), icon: 'm9 11-6 6v3h9l3-3M22 12l-4.6 4.6a2 2 0 0 1-2.8 0l-5.2-5.2a2 2 0 0 1 0-2.8L14 4', C: Highlights },
    { id: 'send', label: t('Send books', 'Enviar libros'), icon: 'M4 19.5V5a2 2 0 0 1 2-2h14v18H6.5a2.5 2.5 0 0 1 0-5H20M12 7v6M9 10l3-3 3 3', C: Send },
    { id: 'files', label: t('Files', 'Archivos'), icon: 'M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z', C: Files },
    { id: 'patch', label: 'Patches', icon: 'm16 18 6-6-6-6M8 6l-6 6 6 6', C: Patches },
    { id: 'store', label: t('Store', 'Tienda'), icon: 'M3 9l1.5-5h15L21 9M3 9h18M3 9v11h18V9M9 20v-6h6v6', C: Store },
    { id: 'cfg', label: t('KOReader files', 'Archivos KOReader'), icon: 'M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6', C: Config },
    { id: 'log', label: t('Log', 'Registro'), icon: 'm4 17 6-6-6-6M12 19h8', C: Log },
    { id: 'fonts', label: t('Fonts', 'Fuentes'), icon: 'M4 7V4h16v3M9 20h6M12 4v16', C: Fonts },
  ])
  const mac = navigator.userAgent.includes('Mac')
  const shortcut = mac ? '⌘,' : 'Ctrl+,'
  // ⌘, (Ctrl+, on Linux) opens Settings, like any Mac app
  const onKey = (e: KeyboardEvent) => { if (e.key === ',' && (mac ? e.metaKey : e.ctrlKey)) { e.preventDefault(); open('prefs') } }
  let current = $state('backup')
  // Views stay mounted once opened, so switching is instant and keeps scroll and selection.
  // Stats and highlights mount hidden at launch to prefetch. The log remounts each visit
  // because it streams tail -f.
  let seen = $state(new Set(['backup', 'stats', 'hl']))
  const open = (id: string) => { current = id; if (!seen.has(id)) seen = new Set([...seen, id]) }

  const gb = (kb: number) => dec(kb / 1024 / 1024)
  const check = () => refreshStatus().then(maybeAutoBackup)

  onMount(() => {
    // the window starts hidden (tauri.conf.json) so it never flashes an unstyled frame
    getCurrentWindow().show()
    loadSettings().then(check)
    checkUpdate()
    const t = setInterval(check, 30_000)
    return () => clearInterval(t)
  })

  const drag = (e: MouseEvent) => { if (e.buttons === 1) getCurrentWindow().startDragging() }
</script>

<svelte:window onkeydown={onKey} />

<div class="win">
  <nav class="side" aria-label={t('Sections', 'Secciones')}>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="titlebar" onmousedown={drag}></div>
    <div class="brand">KOBridge</div>
    {#each views as v (v.id)}
      <button class="nav" aria-current={current === v.id} onclick={() => open(v.id)}>
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d={v.icon} /></svg>
        <span class="name">{v.label}</span>
        {#if v.id === 'backup' && app.backup.busy}<span class="spin" aria-label={t('backing up', 'respaldando')}></span>{/if}
      </button>
    {/each}

    <div class="dev num">
      {#if app.checking && !app.status}
        <b><span class="spin"></span>{t('Looking for the Kindle…', 'Buscando la Kindle…')}</b>
        <span>{t('Trying its known IPs over SSH', 'Probando sus IPs conocidas por SSH')}</span>
      {:else if app.status}
        <b><span class="dot"></span>{t('Kindle connected', 'Kindle conectada')}</b>
        <span>{app.status.host}</span>
        <span>{#if app.status.battery !== null}{t('Battery', 'Batería')} {app.status.battery} %{' · '}{/if}{gb(app.status.free_kb)} {t('GB free', 'GB libres')}</span>
      {:else}
        <b><span class="dot off"></span>{t('Kindle offline', 'Kindle sin conexión')}</b>
        <span>{app.offlineHint}</span>
        <details>
          <summary>{t('Details', 'Detalle')}</summary>
          <code class="selectable">{app.offlineReason}</code>
        </details>
        <button class="btn ghost retry" onclick={check}>{t('Retry', 'Reintentar')}</button>
      {/if}
      {#if updater.available}
        <button class="btn upd" disabled={updater.busy || app.backup.busy} title={app.backup.busy ? t('Wait for the backup to finish', 'Esperá a que termine el respaldo') : ''} onclick={installUpdate}>{updateLabel()}</button>
        {#if updater.error}<span class="selectable upd-err">{updater.error}</span>{/if}
      {/if}
      <button class="nav prefs" aria-current={current === 'prefs'} onclick={() => open('prefs')}>
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" /></svg>
        <span class="name">{t('Settings', 'Configuración')}</span>
        <kbd>{shortcut}</kbd>
      </button>
    </div>
  </nav>

  <main>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="titlebar" onmousedown={drag}></div>
    <div class="pane">
      {#each views as v (v.id)}
        {#if v.id === 'log' ? current === 'log' : seen.has(v.id)}
          <div class="view" hidden={v.id !== current}><v.C /></div>
        {/if}
      {/each}
      {#if seen.has('prefs')}<div class="view" hidden={current !== 'prefs'}><Prefs {shortcut} /></div>{/if}
    </div>
  </main>
</div>

<style>
  .win { display: grid; grid-template-columns: 200px 1fr; grid-template-rows: 100%; height: 100%; overflow: hidden }
  .side { position: relative; background: var(--paper); border-right: 1px solid var(--rule); padding: 52px 10px 14px; display: flex; flex-direction: column; gap: 2px; min-height: 0; overflow-y: auto }
  .titlebar { position: absolute; inset: 0 0 auto 0; height: 40px; z-index: 1 }
  .brand { font: 600 15px/1 var(--serif); padding: 0 8px 14px }
  .nav { appearance: none; border: 0; background: none; color: var(--ink-2); font: inherit; text-align: left; display: flex; align-items: center; gap: 10px; padding: 7px 8px; border-radius: 6px }
  .nav svg { width: 16px; height: 16px; flex: none; stroke: currentColor; fill: none; stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round }
  .name { flex: 1 }
  .nav:hover { background: var(--rule); color: var(--ink) }
  .nav[aria-current='true'] { background: var(--ink); color: var(--paper) }
  .spin { width: 10px; height: 10px; border-radius: 50%; border: 1.5px solid currentColor; border-right-color: transparent; animation: r 0.8s linear infinite }
  @keyframes r { to { transform: rotate(360deg) } }
  .dev { margin-top: auto; border-top: 1px solid var(--rule); padding: 12px 8px 0; display: grid; gap: 4px; font-size: 12px; color: var(--ink-2) }
  .dev b { color: var(--ink); font-weight: 600; display: flex; align-items: center; gap: 6px }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--ok) }
  .dot.off { background: var(--bad) }
  details { font-size: 11px }
  summary { cursor: pointer }
  details code { display: block; margin-top: 4px; font-size: 10.5px; white-space: pre-wrap; word-break: break-word }
  .retry { margin-top: 6px; justify-self: start }
  .upd { margin-top: 8px; justify-self: stretch }
  .upd-err { font-size: 11px; color: var(--bad); word-break: break-word }
  .prefs { margin: 8px -8px 0; font-size: 13px }
  kbd { font: 11px var(--sans); color: var(--ink-2) }
  .prefs[aria-current='true'] kbd { color: inherit }
  main { position: relative; background: var(--sheet); min-width: 0; min-height: 0; display: flex; flex-direction: column }
  .pane { flex: 1; overflow: auto; padding: 44px 32px 28px }
</style>
