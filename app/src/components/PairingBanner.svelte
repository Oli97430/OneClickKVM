<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import {
    getPairingModeStatus,
    startPairingMode,
    stopPairingMode,
    type PairingModeView,
  } from "../ipc";
  import { pushNotification } from "./Notifications.svelte";
  import { t } from "../i18n.svelte";

  // Mode d'appairage cote serveur : on affiche le PIN avec un compte a rebours
  // visible. Quand le PIN expire (passe a 0), on bascule sur l'etat "inactif"
  // sans re-emettre un nouveau PIN — le user doit cliquer pour relancer.

  let mode = $state<PairingModeView>({
    active: false,
    pin: null,
    expires_at_ms: null,
  });
  let secondsLeft = $state<number>(0);
  let tickHandle: ReturnType<typeof setInterval> | null = null;

  function recomputeCountdown() {
    if (!mode.active || !mode.expires_at_ms) {
      secondsLeft = 0;
      return;
    }
    const ms = mode.expires_at_ms - Date.now();
    secondsLeft = Math.max(0, Math.ceil(ms / 1000));
    if (secondsLeft === 0) {
      // expire cote frontend : on prefere une UI propre que d'attendre que
      // l'utilisateur recharge.
      mode = { active: false, pin: null, expires_at_ms: null };
    }
  }

  onMount(async () => {
    try {
      mode = await getPairingModeStatus();
    } catch (e) {
      console.warn("getPairingModeStatus failed", e);
    }
    recomputeCountdown();
    tickHandle = setInterval(recomputeCountdown, 250);
  });

  onDestroy(() => {
    if (tickHandle) clearInterval(tickHandle);
  });

  async function openPairing() {
    try {
      mode = await startPairingMode(60);
      recomputeCountdown();
      pushNotification({
        level: "info",
        title: t("pairing.title"),
        body: `PIN: ${mode.pin}`,
      });
    } catch (e) {
      pushNotification({ level: "error", title: "Erreur", body: String(e) });
    }
  }

  async function closePairing() {
    try {
      await stopPairingMode();
      mode = { active: false, pin: null, expires_at_ms: null };
      secondsLeft = 0;
    } catch (e) {
      pushNotification({ level: "error", title: "Erreur", body: String(e) });
    }
  }
</script>

<div class="banner" class:active={mode.active}>
  <div class="header-row">
    <span class="title">🔐 {t("pairing.title")}</span>
    {#if mode.active}
      <button class="ghost" onclick={closePairing}>{t("pairing.close")}</button>
    {:else}
      <button class="primary" onclick={openPairing}>{t("pairing.open")}</button>
    {/if}
  </div>

  {#if mode.active && mode.pin}
    <div class="pin-block">
      <span class="pin-label">{t("pairing.pin_label")}</span>
      <div class="pin">{mode.pin}</div>
      <div class="countdown" class:warn={secondsLeft <= 10}>
        {t("pairing.expires_in")}
        <strong>{secondsLeft}</strong>
        {t("pairing.seconds")}
      </div>
    </div>
  {:else}
    <p class="hint">{t("pairing.inactive_hint")}</p>
  {/if}
</div>

<style>
  .banner {
    border: 1px solid var(--border);
    background: var(--surface);
    border-radius: 12px;
    padding: 0.85rem 1.1rem;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    transition: border-color 200ms ease, background 200ms ease;
  }

  .banner.active {
    border-color: rgba(59, 130, 246, 0.55);
    background: linear-gradient(
      135deg,
      rgba(59, 130, 246, 0.07) 0%,
      rgba(139, 92, 246, 0.07) 100%
    );
  }

  .header-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }

  .title {
    font-size: 0.88rem;
    font-weight: 600;
    color: var(--fg);
  }

  .hint {
    margin: 0;
    font-size: 0.82rem;
    color: var(--fg-muted);
    line-height: 1.5;
  }

  .pin-block {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.35rem;
    padding: 0.4rem 0;
  }

  .pin-label {
    font-size: 0.75rem;
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .pin {
    font-family: "Cascadia Code", "JetBrains Mono", "Consolas", monospace;
    font-size: 2.2rem;
    letter-spacing: 0.4rem;
    font-weight: 700;
    color: var(--fg);
    background: var(--surface-2);
    padding: 0.5rem 1.3rem;
    border-radius: 10px;
    user-select: all;
    animation: pin-pulse 2.2s ease-in-out infinite;
    box-shadow: 0 0 0 0 rgba(59, 130, 246, 0.35);
  }

  @keyframes pin-pulse {
    0%, 100% {
      box-shadow: 0 0 0 0 rgba(59, 130, 246, 0.35);
    }
    50% {
      box-shadow: 0 0 0 8px rgba(59, 130, 246, 0);
    }
  }

  /* Respect user preference for reduced motion. */
  @media (prefers-reduced-motion: reduce) {
    .pin {
      animation: none;
    }
  }

  .countdown {
    font-size: 0.78rem;
    color: var(--fg-muted);
  }

  .countdown.warn {
    color: #fbbf24;
  }

  .countdown strong {
    color: var(--fg);
    font-variant-numeric: tabular-nums;
    margin: 0 0.15rem;
  }

  button.primary {
    background: linear-gradient(135deg, #3b82f6 0%, #6366f1 100%);
    border: none;
    color: white;
    padding: 0.45rem 0.95rem;
    border-radius: 7px;
    cursor: pointer;
    font-size: 0.82rem;
    font-weight: 600;
    transition: filter 120ms ease;
  }

  button.primary:hover {
    filter: brightness(1.1);
  }

  button.ghost {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--fg);
    padding: 0.4rem 0.85rem;
    border-radius: 7px;
    cursor: pointer;
    font-size: 0.82rem;
    transition: background 120ms ease;
  }

  button.ghost:hover {
    background: var(--bg-hover);
  }
</style>
