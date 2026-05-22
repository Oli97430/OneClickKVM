<script lang="ts">
  import { onMount } from "svelte";
  import {
    becomeMaster,
    fingerprintToString,
    isAudioSharing,
    isVideoSharing,
    startAudioShare,
    startListening,
    startVideoShare,
    stopAudioShare,
    stopListening,
    stopMaster,
    stopVideoShare,
    type AppStatus,
  } from "../ipc";
  import { pushNotification } from "./Notifications.svelte";
  import { t } from "../i18n.svelte";

  let { status }: { status: AppStatus | null } = $props();

  let masterActive = $state(false);
  let audioActive = $state(false);
  let videoActive = $state(false);
  let confirmingMaster = $state(false);
  let modalCancelBtn: HTMLButtonElement | null = $state(null);
  let modalConfirmBtn: HTMLButtonElement | null = $state(null);

  onMount(async () => {
    try {
      audioActive = await isAudioSharing();
      videoActive = await isVideoSharing();
    } catch {
      // ignore
    }
  });

  async function toggleListening() {
    if (!status) return;
    try {
      if (status.listening) {
        await stopListening();
      } else {
        await startListening();
      }
    } catch (e) {
      pushNotification({ level: "error", title: "Erreur", body: String(e) });
    }
  }

  function requestToggleMaster() {
    if (masterActive) {
      // Stop master = action sans risque, pas de confirm.
      void doStopMaster();
    } else {
      // Become master = capture clavier/souris globale → confirm.
      confirmingMaster = true;
      // Focus le bouton "Annuler" par défaut (action la moins risquée).
      // Le requestAnimationFrame attend le mount du modal.
      requestAnimationFrame(() => modalCancelBtn?.focus());
    }
  }

  /**
   * Focus trap : si Tab/Shift+Tab sort des 2 boutons du modal, on le ramène
   * à l'intérieur. Ferme avec Escape.
   */
  function handleModalKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      confirmingMaster = false;
      return;
    }
    if (e.key !== "Tab") return;
    const focusables = [modalCancelBtn, modalConfirmBtn].filter(
      (b): b is HTMLButtonElement => b !== null,
    );
    if (focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && active === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  }

  async function doStopMaster() {
    try {
      await stopMaster();
      masterActive = false;
    } catch (e) {
      pushNotification({ level: "error", title: "Erreur master", body: String(e) });
    }
  }

  async function confirmBecomeMaster() {
    confirmingMaster = false;
    try {
      await becomeMaster();
      masterActive = true;
    } catch (e) {
      pushNotification({ level: "error", title: "Erreur master", body: String(e) });
    }
  }

  async function toggleAudio() {
    try {
      if (audioActive) {
        await stopAudioShare();
        audioActive = false;
      } else {
        await startAudioShare();
        audioActive = true;
      }
    } catch (e) {
      pushNotification({ level: "error", title: "Erreur audio", body: String(e) });
    }
  }

  async function toggleVideo() {
    try {
      if (videoActive) {
        await stopVideoShare();
        videoActive = false;
      } else {
        await startVideoShare();
        videoActive = true;
      }
    } catch (e) {
      pushNotification({ level: "error", title: "Erreur video", body: String(e) });
    }
  }
</script>

<div class="status-bar">
  {#if status}
    <div class="row">
      <div class="cell">
        <span class="label">{t("status.host")}</span>
        <span class="value">{status.self_hostname}</span>
      </div>
      <div class="cell">
        <span class="label">{t("status.fingerprint")}</span>
        <span class="value mono">{fingerprintToString(status.self_fingerprint)}</span>
      </div>
      <div class="cell">
        <span class="label">{t("status.connected_peers")}</span>
        <span class="value">{status.connected_peers}</span>
      </div>
      <div class="cell action stack">
        <button
          class="primary {status.listening ? 'on' : 'off'}"
          onclick={toggleListening}
        >
          {status.listening ? t("status.listening_on") : t("status.listening_off")}
        </button>
        <button
          class="primary {masterActive ? 'master-on' : 'master-off'}"
          onclick={requestToggleMaster}
          disabled={!status.listening || status.connected_peers === 0}
        >
          {masterActive ? t("status.master_on") : t("status.master_off")}
        </button>
        <button
          class="primary {audioActive ? 'audio-on' : 'audio-off'}"
          onclick={toggleAudio}
          disabled={!status.listening || status.connected_peers === 0}
        >
          {audioActive ? t("status.audio_on") : t("status.audio_off")}
        </button>
        <button
          class="primary {videoActive ? 'video-on' : 'video-off'}"
          onclick={toggleVideo}
          disabled={!status.listening || status.connected_peers === 0}
          title="Capture et diffuse l'ecran vers les pairs (MJPEG 1280x720 @ 15fps)"
        >
          {videoActive ? "Ecran partage" : "Partager ecran"}
        </button>
      </div>
    </div>
  {:else}
    <div class="loading">Chargement du statut...</div>
  {/if}
</div>

{#if confirmingMaster}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="modal-backdrop"
    role="dialog"
    aria-modal="true"
    aria-labelledby="master-confirm-title"
    onclick={() => (confirmingMaster = false)}
    onkeydown={handleModalKey}
    tabindex="-1"
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="modal" onclick={(e) => e.stopPropagation()} role="document">
      <h3 id="master-confirm-title">{t("master.confirm_title")}</h3>
      <p>{t("master.confirm_body")}</p>
      <div class="modal-actions">
        <button
          class="ghost"
          bind:this={modalCancelBtn}
          onclick={() => (confirmingMaster = false)}
        >
          {t("master.confirm_cancel")}
        </button>
        <button
          class="primary master-off"
          bind:this={modalConfirmBtn}
          onclick={confirmBecomeMaster}
        >
          {t("master.confirm_ok")}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .status-bar {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 1rem 1.25rem;
  }

  .row {
    display: grid;
    grid-template-columns: 1fr 2fr 1fr auto;
    gap: 1.25rem;
    align-items: center;
  }

  .cell {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }

  .label {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-muted);
  }

  .value {
    font-size: 0.95rem;
    font-weight: 500;
  }

  .mono {
    font-family: "Cascadia Code", "JetBrains Mono", "Consolas", monospace;
    font-size: 0.78rem;
    color: var(--fg-muted);
  }

  .action {
    justify-self: end;
  }

  .stack {
    flex-direction: row;
    gap: 0.45rem;
  }

  button.primary {
    border: none;
    color: white;
    padding: 0.5rem 1.1rem;
    border-radius: 8px;
    cursor: pointer;
    font-weight: 600;
    font-size: 0.85rem;
    letter-spacing: 0.01em;
    transition: filter 120ms ease, transform 80ms ease;
  }

  button.primary:hover:not(:disabled) {
    filter: brightness(1.1);
  }

  button.primary:active:not(:disabled) {
    transform: translateY(1px);
  }

  button.primary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  button.primary.on {
    background: linear-gradient(135deg, #22c55e 0%, #16a34a 100%);
    box-shadow: 0 0 0 1px rgba(34, 197, 94, 0.5), 0 4px 14px rgba(34, 197, 94, 0.25);
  }

  button.primary.off {
    background: linear-gradient(135deg, #6b7280 0%, #4b5563 100%);
  }

  button.primary.master-on {
    background: linear-gradient(135deg, #f59e0b 0%, #d97706 100%);
    box-shadow: 0 0 0 1px rgba(245, 158, 11, 0.55), 0 4px 14px rgba(245, 158, 11, 0.25);
  }

  button.primary.master-off {
    background: linear-gradient(135deg, #3b82f6 0%, #6366f1 100%);
  }

  button.primary.audio-on {
    background: linear-gradient(135deg, #ec4899 0%, #be185d 100%);
    box-shadow: 0 0 0 1px rgba(236, 72, 153, 0.5), 0 4px 14px rgba(236, 72, 153, 0.25);
  }

  button.primary.audio-off {
    background: linear-gradient(135deg, #8b5cf6 0%, #6d28d9 100%);
  }

  button.primary.video-on {
    background: linear-gradient(135deg, #10b981 0%, #047857 100%);
    box-shadow: 0 0 0 1px rgba(16, 185, 129, 0.5), 0 4px 14px rgba(16, 185, 129, 0.25);
  }

  button.primary.video-off {
    background: linear-gradient(135deg, #06b6d4 0%, #0891b2 100%);
  }

  .loading {
    color: var(--fg-muted);
    text-align: center;
    padding: 0.5rem;
  }

  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: grid;
    place-items: center;
    z-index: 100;
    backdrop-filter: blur(2px);
  }

  .modal {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 1.4rem 1.6rem;
    max-width: 480px;
    box-shadow: 0 18px 48px rgba(0, 0, 0, 0.45);
  }

  .modal h3 {
    margin: 0 0 0.6rem;
    font-size: 1rem;
  }

  .modal p {
    margin: 0;
    font-size: 0.88rem;
    color: var(--fg-muted);
    line-height: 1.55;
  }

  .modal-actions {
    margin-top: 1.2rem;
    display: flex;
    justify-content: flex-end;
    gap: 0.55rem;
  }

  .modal button.ghost {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--fg);
    padding: 0.45rem 1rem;
    border-radius: 7px;
    cursor: pointer;
    font-size: 0.85rem;
  }

  .modal button.ghost:hover {
    background: var(--bg-hover);
  }
</style>
