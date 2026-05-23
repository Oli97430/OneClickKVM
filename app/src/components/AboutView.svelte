<script lang="ts">
  import { onMount } from "svelte";
  import { fingerprintToString, getAboutInfo, type AboutInfo } from "../ipc";
  import { pushNotification } from "./Notifications.svelte";
  import { t } from "../i18n.svelte";

  let info = $state<AboutInfo | null>(null);

  onMount(async () => {
    try {
      info = await getAboutInfo();
    } catch (e) {
      pushNotification({
        level: "error",
        title: "Erreur",
        body: String(e),
      });
    }
  });

  async function copyFingerprint() {
    if (!info) return;
    const fp = fingerprintToString(info.self_fingerprint);
    try {
      await navigator.clipboard.writeText(fp);
      pushNotification({
        level: "success",
        title: "Empreinte copiee",
        body: fp,
      });
    } catch (e) {
      pushNotification({
        level: "warn",
        title: "Copie impossible",
        body: String(e),
      });
    }
  }
</script>

<div class="about">
  {#if !info}
    <div class="loading">Chargement...</div>
  {:else}
    <div class="hero">
      <div class="logo">OK</div>
      <div>
        <h2>{info.app_name}</h2>
        <p class="version">Version {info.version}</p>
      </div>
    </div>

    <dl class="props">
      <dt>Hote</dt>
      <dd>{info.self_hostname}</dd>

      <dt>Empreinte (cryptographique)</dt>
      <dd class="mono fp">
        {fingerprintToString(info.self_fingerprint)}
        <button class="copy" onclick={copyFingerprint} title="Copier">⧉</button>
      </dd>

      <dt>Port TCP d'ecoute</dt>
      <dd class="mono">{info.tcp_port}</dd>

      <dt>Dossier d'arrivee fichiers</dt>
      <dd class="mono path">{info.inbox_dir}</dd>

      <dt>Target Rust</dt>
      <dd class="mono">{info.rust_target}</dd>

      <dt>Licence</dt>
      <dd>{info.license}</dd>

      <dt>Encodage H.264 (actif)</dt>
      <dd>
        <span class="mono">{info.mft_backend_active}</span>
        {#if info.h264_encoders.length > 0}
          <details class="encoders">
            <summary>{info.h264_encoders.length} encodeur(s) détecté(s)</summary>
            <ul>
              {#each info.h264_encoders as enc}
                <li>
                  <span class="enc-tag" class:hw={enc.is_hardware}>
                    {enc.is_hardware ? "HW" : "SW"}
                  </span>
                  <span class="enc-mode" class:async={enc.is_async_mode}>
                    {enc.is_async_mode ? "async" : "sync"}
                  </span>
                  {enc.friendly_name}
                </li>
              {/each}
            </ul>
            <p class="hint">
              Les MFT hardware en mode <em>async</em> ne sont pas
              utilisables tant que V3.3.1 n'est pas livré (wrapping event
              loop). Sur ce PC, c'est probablement la cause si l'encodeur
              actif est software malgré la présence d'un GPU compatible.
            </p>
          </details>
        {/if}
      </dd>
    </dl>

    <div class="links">
      <h3>{t("about.shortcuts.title")}</h3>
      <ul class="shortcuts">
        <li>
          <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Win</kbd>+<kbd>0</kbd>
          <span>{t("about.shortcuts.return")}</span>
        </li>
        <li>
          <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Win</kbd>+<kbd>1</kbd>..<kbd>9</kbd>
          <span>{t("about.shortcuts.target_n")}</span>
        </li>
        <li>
          <span class="edge-icon">⇆</span>
          <span>{t("about.shortcuts.edge")}</span>
        </li>
      </ul>
    </div>

    <div class="links">
      <h3>{t("about.security.title")}</h3>
      <ul>
        <li>Tout le trafic est chiffre <strong>AES-256-GCM</strong></li>
        <li>Identite long-terme <strong>Ed25519</strong></li>
        <li>Echange de cles par session <strong>X25519 ECDH</strong> (Perfect Forward Secrecy)</li>
        <li>Verification BLAKE3 sur tous les fichiers transferes</li>
      </ul>
      <p class="note">
        Empreinte est partagee a l'oral / par signaler pour verifier l'identite
        d'un pair (style WireGuard / SSH).
      </p>
    </div>
  {/if}
</div>

<style>
  .loading {
    color: var(--fg-muted);
    text-align: center;
    padding: 2rem;
  }

  .about {
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
  }

  .hero {
    display: flex;
    align-items: center;
    gap: 1rem;
  }

  .logo {
    width: 56px;
    height: 56px;
    border-radius: 14px;
    background: linear-gradient(135deg, #3b82f6 0%, #8b5cf6 100%);
    color: white;
    font-weight: 700;
    display: grid;
    place-items: center;
    font-size: 1.1rem;
    box-shadow: 0 6px 18px rgba(59, 130, 246, 0.35);
  }

  h2 {
    margin: 0;
    font-size: 1.2rem;
  }

  .version {
    margin: 0.2rem 0 0;
    font-size: 0.85rem;
    color: var(--fg-muted);
  }

  .props {
    display: grid;
    grid-template-columns: 220px 1fr;
    gap: 0.55rem 1rem;
    margin: 0;
  }

  dt {
    color: var(--fg-muted);
    font-size: 0.82rem;
    align-self: center;
  }

  dd {
    margin: 0;
    font-size: 0.9rem;
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }

  .mono {
    font-family: "Cascadia Code", "JetBrains Mono", "Consolas", monospace;
    font-size: 0.82rem;
  }

  .fp {
    background: var(--surface-2);
    padding: 0.3rem 0.5rem;
    border-radius: 6px;
  }

  .path {
    word-break: break-all;
    background: var(--surface-2);
    padding: 0.3rem 0.5rem;
    border-radius: 6px;
  }

  button.copy {
    background: transparent;
    border: none;
    color: var(--fg-muted);
    cursor: pointer;
    padding: 0 0.4rem;
    font-size: 0.95rem;
  }

  button.copy:hover {
    color: var(--accent);
  }

  .links h3 {
    margin: 0 0 0.6rem;
    font-size: 0.85rem;
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .links ul {
    margin: 0;
    padding-left: 1.3rem;
    font-size: 0.88rem;
    line-height: 1.7;
  }

  .links strong {
    color: var(--accent);
  }

  .note {
    margin: 0.85rem 0 0;
    font-size: 0.82rem;
    color: var(--fg-muted);
    font-style: italic;
  }

  ul.shortcuts {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
    font-size: 0.88rem;
  }

  ul.shortcuts li {
    display: flex;
    align-items: center;
    gap: 0.7rem;
    flex-wrap: wrap;
  }

  ul.shortcuts span {
    color: var(--fg-muted);
  }

  kbd {
    display: inline-block;
    padding: 0.15rem 0.45rem;
    border: 1px solid var(--border);
    border-bottom-width: 2px;
    border-radius: 4px;
    background: var(--surface-2);
    font-family: "Cascadia Code", "Consolas", monospace;
    font-size: 0.78rem;
    color: var(--fg);
    min-width: 1.4rem;
    text-align: center;
  }

  .edge-icon {
    display: inline-grid;
    place-items: center;
    width: 1.6rem;
    height: 1.6rem;
    border-radius: 6px;
    background: linear-gradient(135deg, #3b82f6 0%, #6366f1 100%);
    color: white !important;
    font-weight: 700;
  }

  .encoders {
    margin-top: 0.45rem;
    font-size: 0.8rem;
  }

  .encoders summary {
    cursor: pointer;
    color: var(--fg-muted);
  }

  .encoders ul {
    list-style: none;
    padding: 0.4rem 0 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .enc-tag {
    display: inline-block;
    padding: 0 0.35rem;
    border-radius: 3px;
    font-family: "Cascadia Code", "Consolas", monospace;
    font-size: 0.68rem;
    font-weight: 700;
    background: var(--surface-2);
    color: var(--fg-muted);
    margin-right: 0.45rem;
    min-width: 1.8rem;
    text-align: center;
  }

  .enc-tag.hw {
    background: rgba(34, 197, 94, 0.18);
    color: #86efac;
  }

  .enc-mode {
    display: inline-block;
    padding: 0 0.35rem;
    border-radius: 3px;
    font-family: "Cascadia Code", "Consolas", monospace;
    font-size: 0.66rem;
    background: var(--surface-2);
    color: var(--fg-muted);
    margin-right: 0.45rem;
  }

  .enc-mode.async {
    background: rgba(245, 158, 11, 0.18);
    color: #fcd34d;
  }

  .encoders .hint {
    margin: 0.5rem 0 0;
    font-size: 0.75rem;
    color: var(--fg-muted);
    font-style: italic;
    line-height: 1.4;
  }

  .encoders .hint em {
    font-weight: 600;
    color: var(--fg);
  }
</style>
