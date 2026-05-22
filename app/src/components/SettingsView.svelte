<script lang="ts">
  import { onMount } from "svelte";
  import {
    getAppConfig,
    listLocalScreens,
    openConfigDir,
    openInboxDir,
    resetAllSettings,
    setAppConfig,
    type AppConfig,
    type ScreenView,
  } from "../ipc";
  import { pushNotification } from "./Notifications.svelte";
  import { setLang, t } from "../i18n.svelte";
  import { setTheme } from "../theme.svelte";

  let cfg = $state<AppConfig | null>(null);
  let saving = $state(false);
  let confirmingReset = $state(false);
  let screens = $state<ScreenView[]>([]);

  // Switch live de la langue et du theme dans le formulaire pour preview
  // immediat — sans avoir besoin de cliquer "Enregistrer".
  $effect(() => {
    if (cfg?.language) {
      setLang(cfg.language);
    }
  });
  $effect(() => {
    if (cfg?.theme) {
      setTheme(cfg.theme);
    }
  });

  onMount(async () => {
    try {
      cfg = await getAppConfig();
    } catch (e) {
      pushNotification({
        level: "error",
        title: "Erreur chargement config",
        body: String(e),
      });
    }
    try {
      screens = await listLocalScreens();
    } catch (e) {
      console.warn("listLocalScreens failed", e);
    }
  });

  async function save() {
    if (!cfg) return;
    saving = true;
    try {
      await setAppConfig(cfg);
      pushNotification({
        level: "success",
        title: "Parametres enregistres",
        body: "Certains changements requierent un redemarrage.",
      });
    } catch (e) {
      pushNotification({ level: "error", title: "Echec", body: String(e) });
    } finally {
      saving = false;
    }
  }

  async function doReset() {
    try {
      await resetAllSettings();
      pushNotification({
        level: "warn",
        title: "Reinitialisation effectuee",
        body: "Tout a ete efface. Redemarre l'application.",
      });
      confirmingReset = false;
    } catch (e) {
      pushNotification({ level: "error", title: "Reset echec", body: String(e) });
    }
  }

  async function openConfig() {
    try {
      await openConfigDir();
    } catch (e) {
      pushNotification({ level: "error", title: "Echec ouverture", body: String(e) });
    }
  }

  async function openInbox() {
    try {
      await openInboxDir();
    } catch (e) {
      pushNotification({ level: "error", title: "Echec ouverture", body: String(e) });
    }
  }
</script>

<div class="settings">
  {#if !cfg}
    <div class="loading">Chargement...</div>
  {:else}
    <div class="form">
      <label class="field">
        <span class="label">{t("settings.language")}</span>
        <select bind:value={cfg.language}>
          <option value="fr">Francais</option>
          <option value="en">English</option>
          <option value="de">Deutsch</option>
          <option value="es">Espanol</option>
          <option value="it">Italiano</option>
          <option value="pt">Portugues</option>
          <option value="nl">Nederlands</option>
          <option value="ja">日本語</option>
          <option value="zh">中文</option>
        </select>
      </label>

      <label class="field">
        <span class="label">{t("settings.theme")}</span>
        <select bind:value={cfg.theme}>
          <option value="System">{t("settings.theme.system")}</option>
          <option value="Light">{t("settings.theme.light")}</option>
          <option value="Dark">{t("settings.theme.dark")}</option>
        </select>
      </label>

      <label class="field">
        <span class="label">{t("settings.bind_addr")}</span>
        <input type="text" bind:value={cfg.bind_addr} placeholder="[::]" />
        <span class="hint">Default: <code>[::]</code> (dual-stack IPv6/IPv4)</span>
      </label>

      <label class="field">
        <span class="label">{t("settings.h264_backend")}</span>
        <select bind:value={cfg.h264_backend}>
          <option value="MediaFoundation">{t("settings.h264_backend.mf")}</option>
          <option value="Openh264">{t("settings.h264_backend.openh264")}</option>
        </select>
        <span class="hint">{t("settings.h264_backend.hint")}</span>
      </label>

      {#if screens.length > 1}
        <label class="field">
          <span class="label">{t("settings.screen")}</span>
          <select bind:value={cfg.video_screen_idx}>
            {#each screens as s}
              <option value={s.index}>
                #{s.index} — {s.width_px}×{s.height_px}
                {s.is_primary ? `(${t("settings.screen.primary")})` : ""}
              </option>
            {/each}
          </select>
          <span class="hint">{t("settings.screen.hint")}</span>
        </label>
      {/if}

      <label class="field-row">
        <input type="checkbox" bind:checked={cfg.autostart} />
        <span>{t("settings.autostart")}</span>
      </label>

      <label class="field-row">
        <input type="checkbox" bind:checked={cfg.start_minimized} />
        <span>{t("settings.start_minimized")}</span>
      </label>

      <label class="field-row">
        <input type="checkbox" bind:checked={cfg.discovery_mdns} />
        <span>{t("settings.mdns")}</span>
      </label>

      <label class="field-row">
        <input type="checkbox" bind:checked={cfg.discovery_broadcast} />
        <span>{t("settings.broadcast")}</span>
      </label>

      <label class="field-row">
        <input type="checkbox" bind:checked={cfg.redact_logs} />
        <span>{t("settings.redact_logs")}</span>
      </label>

      <div class="actions">
        <button class="primary" onclick={save} disabled={saving}>
          {saving ? t("settings.saving") : t("settings.save")}
        </button>
      </div>

      <div class="folders-zone">
        <h3>{t("settings.folders.title")}</h3>
        <div class="folder-actions">
          <button class="ghost" onclick={openConfig}>
            📂 {t("settings.folders.open_config")}
          </button>
          <button class="ghost" onclick={openInbox}>
            📥 {t("settings.folders.open_inbox")}
          </button>
        </div>
        <p class="hint">{t("settings.folders.hint")}</p>
      </div>

      <div class="danger-zone">
        <h3>{t("settings.danger.title")}</h3>
        {#if confirmingReset}
          <p class="warn-text">
            Cela supprime <strong>tous</strong> les pairs appaires, la configuration,
            et regenere une identite Ed25519. Les pairs distants verront un
            avertissement "empreinte modifiee" si vous reconnectez.
          </p>
          <div class="actions">
            <button class="ghost" onclick={() => (confirmingReset = false)}>
              {t("peer.unpair_cancel")}
            </button>
            <button class="danger" onclick={doReset}>{t("peer.unpair_confirm")}</button>
          </div>
        {:else}
          <button class="danger" onclick={() => (confirmingReset = true)}>
            {t("settings.danger.reset")}
          </button>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .loading {
    color: var(--fg-muted);
    text-align: center;
    padding: 2rem;
  }

  .form {
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
  }

  .field,
  .field-row {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  .field-row {
    flex-direction: row;
    align-items: center;
    gap: 0.55rem;
    cursor: pointer;
  }

  .label {
    font-size: 0.78rem;
    color: var(--fg-muted);
  }

  select,
  input[type="text"] {
    background: var(--surface-2);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.45rem 0.65rem;
    font-size: 0.88rem;
    font-family: inherit;
  }

  select:focus,
  input[type="text"]:focus {
    outline: none;
    border-color: var(--accent);
  }

  input[type="checkbox"] {
    accent-color: var(--accent);
    width: 16px;
    height: 16px;
    cursor: pointer;
  }

  .hint {
    font-size: 0.75rem;
    color: var(--fg-muted);
  }

  code {
    background: var(--surface-2);
    padding: 0 0.3rem;
    border-radius: 3px;
    font-family: "Cascadia Code", "Consolas", monospace;
  }

  .actions {
    display: flex;
    gap: 0.55rem;
    justify-content: flex-end;
    margin-top: 0.5rem;
  }

  button {
    padding: 0.5rem 1rem;
    border-radius: 7px;
    cursor: pointer;
    font-size: 0.88rem;
    font-weight: 500;
    font-family: inherit;
    border: 1px solid var(--border);
    background: transparent;
    color: var(--fg);
    transition: filter 120ms ease, background 120ms ease;
  }

  button.ghost:hover {
    background: var(--bg-hover);
  }

  button.primary {
    background: linear-gradient(135deg, #3b82f6 0%, #6366f1 100%);
    border: none;
    color: white;
    font-weight: 600;
  }

  button.primary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  button.primary:hover:not(:disabled) {
    filter: brightness(1.1);
  }

  button.danger {
    background: linear-gradient(135deg, #ef4444 0%, #b91c1c 100%);
    border: none;
    color: white;
    font-weight: 600;
  }

  button.danger:hover {
    filter: brightness(1.1);
  }

  .folders-zone {
    margin-top: 1.2rem;
    padding: 0.85rem 1rem;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--surface-2);
  }

  .folders-zone h3 {
    margin: 0 0 0.55rem;
    font-size: 0.85rem;
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .folder-actions {
    display: flex;
    gap: 0.55rem;
    flex-wrap: wrap;
  }

  .folder-actions button.ghost {
    border-color: var(--border);
    background: transparent;
  }

  .folder-actions button.ghost:hover {
    background: var(--bg-hover);
  }

  .folders-zone .hint {
    margin: 0.6rem 0 0;
    font-size: 0.78rem;
    color: var(--fg-muted);
    line-height: 1.5;
  }

  .danger-zone {
    margin-top: 1.5rem;
    padding: 1rem;
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: 10px;
    background: rgba(239, 68, 68, 0.05);
  }

  .danger-zone h3 {
    margin: 0 0 0.5rem;
    font-size: 0.85rem;
    color: #fca5a5;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .warn-text {
    margin: 0.5rem 0 0.9rem;
    font-size: 0.85rem;
    color: var(--fg-muted);
    line-height: 1.5;
  }
</style>
