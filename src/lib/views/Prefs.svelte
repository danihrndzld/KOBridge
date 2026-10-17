<script lang="ts">
  import { onMount } from 'svelte'
  import { api, errText, type SshInfo } from '../api'
  import { app, changeLang, explain, refreshStatus, saveSettings, updater, checkUpdate, installUpdate, updateLabel } from '../store.svelte'
  import { getVersion } from '@tauri-apps/api/app'
  import { i18n, t } from '../i18n.svelte'

  let { shortcut }: { shortcut: string } = $props()

  let info = $state<SshInfo | null>(null)
  let busy = $state('')
  let msg = $state<{ kind: 'ok' | 'bad'; text: string } | null>(null)
  let ip = $state('')
  let port = $state(2222)
  let copied = $state(false)

  let version = $state('')
  const load = async () => (info = await api.sshSetupInfo())
  onMount(() => { load(); getVersion().then((v) => (version = v)) })

  async function act(name: string, f: () => Promise<string | void>) {
    busy = name
    msg = null
    try {
      const ok = await f()
      if (ok) msg = { kind: 'ok', text: ok }
    } catch (e) {
      msg = { kind: 'bad', text: errText(e) }
    } finally {
      busy = ''
      await load()
    }
  }
  const createKey = () => act('key', async () => { await api.sshCreateKey(); return t('Created the key.', 'Creé la llave.') })
  const installUsb = () => act('usb', async () => {
    const p = await api.sshInstallUsb()
    return t(`Added the key to ${p}. Eject the Kindle before you unplug it.`, `Agregué la llave a ${p}. Expulsá la Kindle antes de desconectarla.`)
  })
  const addHost = () => act('host', async () => {
    await api.sshAddHost(ip, port)
    return t('Added kindle to ~/.ssh/config. A copy of the old file is in ~/.ssh/config.kob.bak.', 'Agregué kindle a ~/.ssh/config. La copia del archivo anterior quedó en ~/.ssh/config.kob.bak.')
  })
  const test = () => act('test', async () => {
    try {
      const v = await api.sshTest()
      refreshStatus()
      return t(`Connected. KOReader ${v}.`, `Conectado. KOReader ${v}.`)
    } catch (e) {
      const raw = errText(e)
      throw `${explain(raw)} ${raw}`
    }
  })
  async function copyKey() {
    if (!info?.public_key) return
    await navigator.clipboard.writeText(info.public_key)
    copied = true
    setTimeout(() => (copied = false), 1500)
  }
  const keyReady = $derived(!!info?.public_key)
  const mac = navigator.userAgent.includes('Mac')
  const clients = $derived([{ id: '', label: t('Auto', 'Automática') }, ...['KindleMTP', 'OpenMTP', 'Android File Transfer'].map((id) => ({ id, label: id }))])
</script>

<div class="stack">
  <div class="head">
    <div>
      <h1>{t('Settings', 'Configuración')}</h1>
      <p class="sub">{t(`${shortcut} opens this view from anywhere in the app.`, `${shortcut} abre esta vista desde cualquier parte de la app.`)}</p>
    </div>
  </div>

  <div class="list">
    <div class="item">
      <div class="grow"><div>{t('Language', 'Idioma')}</div><div class="sub">{t('Buttons, messages and dates', 'Botones, mensajes y fechas')}</div></div>
      <div class="seg" role="radiogroup" aria-label={t('Language', 'Idioma')}>
        <button role="radio" aria-checked={i18n.lang === 'en'} onclick={() => changeLang('en')}>English</button>
        <button role="radio" aria-checked={i18n.lang === 'es'} onclick={() => changeLang('es')}>Español</button>
      </div>
    </div>
    <div class="item">
      <div class="grow">
        <div>{t('Version', 'Versión')} <span class="num">{version}</span></div>
        <div class="sub">
          {#if updater.error}<span class="selectable bad-text">{updater.error}</span>
          {:else if updater.available}{t(`Version ${updater.available.version} is available. The app restarts after the update.`, `La versión ${updater.available.version} está disponible. La app se reinicia después de actualizar.`)}
          {:else if updater.checked}{t('This is the latest version.', 'Es la última versión.')}
          {:else}{t('The app checks GitHub for a new version at launch.', 'La app busca una versión nueva en GitHub al abrir.')}{/if}
        </div>
      </div>
      {#if updater.available}
        <button class="btn" disabled={updater.busy || app.backup.busy} onclick={installUpdate}>{updateLabel()}</button>
      {:else}
        <button class="btn ghost" disabled={updater.checking} onclick={() => checkUpdate(true)}>{updater.checking ? t('Checking…', 'Buscando…') : t('Check for updates', 'Buscar actualizaciones')}</button>
      {/if}
    </div>
    {#if mac}
      <div class="item">
        <div class="grow"><div>{t('MTP app', 'App MTP')}</div><div class="sub">{t('Opens after Files → Cable → Connect. Auto picks the first one installed in Applications.', 'Se abre después de Archivos → Cable → Conectar. Automática elige la primera instalada en Aplicaciones.')}</div></div>
        <div class="seg" role="radiogroup" aria-label={t('MTP app', 'App MTP')}>
          {#each clients as c (c.id)}
            <button role="radio" aria-checked={(app.settings?.mtp_client ?? '') === c.id} onclick={() => saveSettings({ mtp_client: c.id })}>{c.label}</button>
          {/each}
        </div>
      </div>
    {/if}
  </div>

  <div>
    <div class="row" style="justify-content:space-between; margin-bottom:8px">
      <p class="label">{t('SSH connection', 'Conexión SSH')}</p>
      <button class="link" disabled={!!busy} onclick={load}>{t('Check again', 'Volver a revisar')}</button>
    </div>
    {#if msg}<div class="banner {msg.kind}" style="margin-bottom:12px"><span class="selectable">{msg.text}</span><button class="link" onclick={() => (msg = null)}>{t('Close', 'Cerrar')}</button></div>{/if}

    <div class="list">
      <div class="item step">
        <span class="n">1</span>
        <div class="grow">
          <div>{t('Start the SSH server in KOReader', 'Activá el servidor SSH en KOReader')}</div>
          <div class="sub">{t('On the Kindle, open Tools (the gear icon), then Network, SSH server, Start SSH server. It listens on port 2222, and Network info shows the IP.', 'En la Kindle abrí Herramientas (el engranaje), después Red, Servidor SSH, Iniciar servidor SSH. Escucha en el puerto 2222, y Información de red muestra la IP.')}</div>
        </div>
        {#if app.status}<span class="pill ok">{t('Running', 'Activo')}</span>{/if}
      </div>

      <div class="item step">
        <span class="n">2</span>
        <div class="grow">
          <div>{t('SSH key', 'Llave SSH')}</div>
          <code class="sub selectable">{info?.key_path ?? ''}</code>
          {#if info?.public_key}
            <code class="pub selectable">{info.public_key}</code>
            <div class="sub">
              {#if info.usb}{t(`The Kindle is mounted at ${info.usb}.`, `La Kindle está montada en ${info.usb}.`)}
              {:else}{t('Connect the Kindle over USB to install the key, or paste it into koreader/settings/SSH/authorized_keys on the Kindle.', 'Conectá la Kindle por USB para instalar la llave, o pegala en koreader/settings/SSH/authorized_keys de la Kindle.')}{/if}
            </div>
          {/if}
        </div>
        <div class="acts">
          {#if info && !keyReady}
            <button class="btn" disabled={!!busy} onclick={createKey}>{busy === 'key' ? t('Creating…', 'Creando…') : t('Create key', 'Crear llave')}</button>
          {:else if info}
            <button class="btn ghost" onclick={copyKey}>{copied ? t('Copied', 'Copiada') : t('Copy public key', 'Copiar llave pública')}</button>
            <button class="btn ghost" disabled={!info.usb || !!busy} onclick={installUsb}>{busy === 'usb' ? t('Installing…', 'Instalando…') : t('Install over USB', 'Instalar por USB')}</button>
          {/if}
        </div>
      </div>

      <div class="item step">
        <span class="n">3</span>
        <div class="grow">
          <div><code>~/.ssh/config</code></div>
          {#if info?.has_host}
            <div class="sub num">{info.hostname ? `kindle → ${info.user}@${info.hostname}:${info.port}, ${info.identity}` : t('kindle is configured, but none of its Match probes answers right now.', 'kindle está configurada, pero ahora no responde ninguna de sus pruebas Match.')}</div>
          {:else if info}
            <div class="sub">{t('Add the Kindle as kindle. The app puts the entry at the top of the file and saves a copy of the old file first.', 'Agregá la Kindle como kindle. La app pone la entrada al principio del archivo y antes guarda una copia del anterior.')}</div>
            <form class="field" style="margin-top:8px" onsubmit={(e) => { e.preventDefault(); addHost() }}>
              <input bind:value={ip} placeholder="192.168.1.50" aria-label={t('Kindle IP address', 'IP de la Kindle')} spellcheck="false" />
              <input class="port num" type="number" bind:value={port} min="1" max="65535" aria-label={t('Port', 'Puerto')} />
              <button class="btn" disabled={!ip.trim() || !!busy || !keyReady}>{busy === 'host' ? t('Adding…', 'Agregando…') : t('Add', 'Agregar')}</button>
            </form>
            {#if !keyReady}<div class="sub">{t('Create the key in step 2 first.', 'Primero creá la llave en el paso 2.')}</div>{/if}
          {/if}
        </div>
        <div class="acts">
          {#if info?.has_host}<span class="pill ok">{t('Ready', 'Listo')}</span>{/if}
          <button class="btn ghost" onclick={() => api.openSshConfig().catch((e) => (msg = { kind: 'bad', text: errText(e) }))}>{t('Open file', 'Abrir archivo')}</button>
        </div>
      </div>

      <div class="item step">
        <span class="n">4</span>
        <div class="grow">
          <div>{t('Test the connection', 'Probá la conexión')}</div>
          <div class="sub">{t('The first connection saves the fingerprint of the Kindle in ~/.ssh/known_hosts, so later connections can check it.', 'La primera conexión guarda la huella de la Kindle en ~/.ssh/known_hosts, así las siguientes la pueden verificar.')}</div>
        </div>
        <button class="btn" disabled={!info?.has_host || !!busy} onclick={test}>{busy === 'test' ? t('Connecting…', 'Conectando…') : t('Test', 'Probar')}</button>
      </div>
    </div>
  </div>
</div>

<style>
  .seg { display: flex; gap: 2px; padding: 2px; border: 1px solid var(--rule); border-radius: 8px; background: var(--paper) }
  .seg button { appearance: none; border: 0; background: none; font: inherit; font-size: 12.5px; color: var(--ink-2); padding: 5px 12px; border-radius: 6px }
  .seg button:hover { color: var(--ink) }
  .seg button[aria-checked='true'] { background: var(--sheet); color: var(--ink); box-shadow: 0 1px 2px rgb(0 0 0 / .12) }
  .step { align-items: flex-start }
  .step .grow { display: grid; gap: 4px; min-width: 0 }
  .step .sub { margin: 0 }
  .n { flex: none; width: 22px; height: 22px; border-radius: 50%; background: var(--paper); border: 1px solid var(--rule); display: grid; place-items: center; font-size: 11.5px; color: var(--ink-2) }
  .pub { display: block; font: 11px/1.45 var(--mono); padding: 8px 10px; background: var(--paper); border-radius: 6px; word-break: break-all }
  .acts { display: flex; gap: 8px; align-items: center; flex: none }
  .port { flex: 0 0 90px !important }
  .link:disabled { opacity: .45 }
  .bad-text { color: var(--bad) }
</style>
