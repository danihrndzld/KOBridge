<script lang="ts">
  import { onMount } from 'svelte'
  import { ask } from '@tauri-apps/plugin-dialog'
  import { api, errText, type UsbStatus } from './api'
  import { app, explain } from './store.svelte'
  import { t } from './i18n.svelte'
  import { fs, load } from './files.svelte'

  const mac = navigator.userAgent.includes('Mac')
  let root = $state<HTMLDivElement>()
  let usb = $state<UsbStatus | null>(null)
  let busy = $state<'' | 'connect' | 'disconnect' | 'reset'>('')
  let error = $state('')
  let last: 'connect' | 'disconnect' | 'reset' = 'connect'
  let checking = false

  // NotCharging also fires on a full battery with the cable in, so only a link no computer holds counts
  const unplugged = (s: UsbStatus | null) => s?.state === 'connected' && !s.cable && s.udc_state !== 'configured'

  async function check() {
    if (checking || busy) return
    checking = true
    try { const s = await api.usb('status'); if (!busy) usb = s } catch { /* the Kindle went offline; app.status shows it */ }
    finally { checking = false }
    if (unplugged(usb)) act('disconnect')
  }

  async function act(op: 'connect' | 'disconnect' | 'reset') {
    busy = last = op
    error = ''
    try {
      usb = await api.usb(op)
    } catch (e) {
      // ssh failures get the same hint as the status bar; step errors pass through
      error = explain(errText(e), errText(e))
      try { usb = await api.usb('status') } catch { /* keep the last status */ }
    } finally {
      busy = ''
    }
    if (op !== 'connect') load(fs.cwd)
  }

  async function reset() {
    const ok = await ask(t('Restart the Kindle\'s MTP responder (tizen-mtp) and stop any USB transfer?', '¿Reiniciar el responder MTP de la Kindle (tizen-mtp) y detener cualquier transferencia USB?'),
      { title: t('Reset USB', 'Reiniciar USB'), kind: 'warning', okLabel: t('Reset', 'Reiniciar'), cancelLabel: t('Cancel', 'Cancelar') })
    if (ok) act('reset')
  }

  async function openClient() {
    try { await api.usbOpen() } catch (e) { error = errText(e) }
  }

  // every 5 s while the Files view is visible, except on a Kindle that can't do it
  onMount(() => {
    const id = setInterval(() => {
      if (app.status && root?.checkVisibility() && (!usb || usb.supported || usb.code === 'no_responder')) check()
    }, 5000)
    return () => clearInterval(id)
  })
  $effect(() => { if (app.status && !usb) check() })

  const clients = $derived(`KindleMTP, OpenMTP ${t('or', 'o')} Android File Transfer`)
</script>

{#if app.status}
  <div class="list" bind:this={root}>
    <div class="item">
      <div class="grow">
        <p class="label">{t('Cable', 'Cable')}</p>
        {#if busy === 'connect'}
          <p class="msg row"><span class="spin" aria-hidden="true"></span>{t('Connecting… waiting for the computer (up to 20 s)', 'Conectando… esperando a la computadora (hasta 20 s)')}</p>
        {:else if busy === 'reset'}
          <p class="msg row"><span class="spin" aria-hidden="true"></span>{t('Resetting the USB responder…', 'Reiniciando el responder USB…')}</p>
        {:else if error}
          <p class="msg bad selectable">{error}</p>
        {:else if !usb}
          <p class="msg row"><span class="spin" aria-hidden="true"></span>{t('Checking USB…', 'Revisando USB…')}</p>
        {:else if !usb.supported}
          <p class="msg">{usb.reason}</p>
        {:else if unplugged(usb)}
          <p class="msg">{t('The cable was unplugged. Disconnecting…', 'Se desconectó el cable. Desconectando…')}</p>
        {:else if usb.state === 'connected'}
          <p class="msg ok">{t('Connected. This computer can see the Kindle\'s files over the cable.', 'Conectada. Esta computadora ve los archivos de la Kindle por el cable.')}</p>
        {:else if usb.state === 'busy'}
          <p class="msg">{t('The KOReader plugin is running a USB transfer. Stop it on the Kindle, or disconnect it here.', 'El plugin de KOReader está haciendo una transferencia USB. Detenela en la Kindle o desconectala acá.')}</p>
        {:else if !usb.cable}
          <p class="msg">{t('No power on the cable. Plug the Kindle into this computer with a USB data cable.', 'El cable no da energía. Conectá la Kindle a esta computadora con un cable USB de datos.')}</p>
        {:else}
          <p class="msg">{t('Power on the cable (charger or computer). Connect to browse the Kindle from an MTP app.', 'Hay energía en el cable (cargador o computadora). Conectá para ver la Kindle desde una app MTP.')}</p>
        {/if}
      </div>
      <div class="row">
        {#if error && !busy}
          <button class="btn ghost" onclick={() => (last === 'reset' ? reset() : act(last))}>{t('Retry', 'Reintentar')}</button>
          <button class="btn ghost" onclick={reset}>{t('Reset…', 'Reiniciar…')}</button>
        {:else if usb?.supported && (usb.state !== 'idle' || busy === 'disconnect')}
          {#if usb.state === 'connected' && usb.client}
            <button class="btn ghost" onclick={openClient}>{t('Open', 'Abrir')} {usb.client}</button>
          {/if}
          <button class="btn" disabled={!!busy} onclick={() => act('disconnect')}>{busy === 'disconnect' ? t('Disconnecting…', 'Desconectando…') : t('Disconnect', 'Desconectar')}</button>
        {:else if usb?.supported && usb.cable}
          <button class="btn" disabled={!!busy} onclick={() => act('connect')}>{busy === 'connect' ? t('Connecting…', 'Conectando…') : t('Connect', 'Conectar')}</button>
        {:else if usb?.code === 'no_responder'}
          <button class="btn ghost" disabled={!!busy} onclick={reset}>{busy === 'reset' ? t('Resetting…', 'Reiniciando…') : t('Reset…', 'Reiniciar…')}</button>
        {/if}
      </div>
    </div>
    {#if usb?.supported && usb.state !== 'busy'}
      <div class="item hints">
        {#if usb.state === 'connected'}
          {#if !usb.client}<p>{t(`No MTP app found. Install ${clients} to browse the Kindle.`, `No encontré ninguna app MTP. Instalá ${clients} para ver la Kindle.`)}</p>{/if}
          {#if mac}<p>{t('If the app says the device is busy, macOS\'s ptpcamerad claimed it first: quit Photos and Image Capture, then reopen the app.', 'Si la app dice que el dispositivo está ocupado, ptpcamerad de macOS lo tomó primero: cerrá Fotos y Captura de Imagen y volvé a abrir la app.')}</p>{/if}
          <p>{t('Disconnect before unplugging, so KOReader sees the new files.', 'Desconectá antes de desenchufar, así KOReader ve los archivos nuevos.')}</p>
        {:else}
          <p>{t('Close the book on the Kindle first: KOReader rewrites its sidecar and would overwrite changes made from the computer.', 'Cerrá el libro en la Kindle primero: KOReader reescribe su sidecar y pisaría los cambios hechos desde la computadora.')}</p>
          {#if usb.plugin}<p>{t('kindlemtp.koplugin is installed: starting from the Kindle (Network → USB file transfer (MTP)) checks that for you.', 'kindlemtp.koplugin está instalado: si arrancás desde la Kindle (Red → USB file transfer (MTP)) eso se revisa solo.')}</p>{/if}
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .msg { margin: 3px 0 0; color: var(--ink) }
  .msg.ok { color: var(--ok) }
  .msg.bad { color: var(--bad) }
  .hints { display: grid; gap: 4px; background: var(--paper); font-size: 12px; color: var(--ink-2) }
  .hints p { margin: 0 }
  .spin { width: 12px; height: 12px; flex: none; border-radius: 50%; border: 2px solid var(--rule); border-top-color: var(--ink); animation: r 0.8s linear infinite }
  @keyframes r { to { transform: rotate(360deg) } }
  @media (prefers-reduced-motion: reduce) { .spin { animation: none } }
</style>
