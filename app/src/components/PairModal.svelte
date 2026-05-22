<script lang="ts" module>
  // Etat de modale module-level (singleton facile).
  let _open = $state(false);
  let _defaultAddr = $state("");

  export function openPairModal(addr?: string) {
    _defaultAddr = addr ?? "";
    _open = true;
  }

  export function isPairModalOpen() {
    return _open;
  }

  export function getDefaultAddr() {
    return _defaultAddr;
  }

  export function closePairModal() {
    _open = false;
  }
</script>

<script lang="ts">
  import { pairWithPeer } from "../ipc";
  import { pushNotification } from "./Notifications.svelte";

  const open = $derived(isPairModalOpen());
  let address = $state("");
  let pin = $state("");
  let busy = $state(false);

  $effect(() => {
    if (open) {
      address = getDefaultAddr();
      pin = "";
    }
  });

  async function submit(e: Event) {
    e.preventDefault();
    if (busy) return;
    busy = true;
    try {
      const res = await pairWithPeer({ address, pin });
      if ("kind" in res && res.kind === "success") {
        pushNotification({
          level: "success",
          title: "Appairage reussi",
          body: `${res.name} ajoute aux pairs connus.`,
        });
        closePairModal();
      } else if ("kind" in res && res.kind === "failure") {
        pushNotification({
          level: "error",
          title: "Echec d'appairage",
          body: res.reason,
        });
      } else {
        // Variante serialisee en snake_case par defaut serde
        const anyRes = res as unknown as { Success?: unknown; Failure?: { reason: string } };
        if (anyRes.Success) {
          pushNotification({
            level: "success",
            title: "Appairage reussi",
            body: "Pair ajoute.",
          });
          closePairModal();
        } else if (anyRes.Failure) {
          pushNotification({
            level: "error",
            title: "Echec d'appairage",
            body: anyRes.Failure.reason,
          });
        }
      }
    } catch (e) {
      pushNotification({
        level: "error",
        title: "Erreur",
        body: String(e),
      });
    } finally {
      busy = false;
    }
  }
</script>

{#if open}
  <div
    class="backdrop"
    onclick={closePairModal}
    role="presentation"
  ></div>
  <div class="modal" role="dialog" aria-modal="true" aria-labelledby="pair-title">
    <h2 id="pair-title">Appairer un pair</h2>
    <form onsubmit={submit}>
      <label>
        <span>Adresse <code>ip:port</code></span>
        <input
          type="text"
          bind:value={address}
          placeholder="192.168.1.42:47101"
          required
          autocomplete="off"
        />
      </label>
      <label>
        <span>PIN <span class="hint">(laisser vide pour TOFU)</span></span>
        <input
          type="text"
          bind:value={pin}
          placeholder="123456"
          inputmode="numeric"
          maxlength="6"
          autocomplete="off"
        />
      </label>
      <div class="actions">
        <button type="button" class="ghost" onclick={closePairModal}>
          Annuler
        </button>
        <button type="submit" class="primary" disabled={busy || !address}>
          {busy ? "Connexion..." : "Appairer"}
        </button>
      </div>
    </form>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    z-index: 900;
    animation: fade-in 120ms ease-out;
  }

  .modal {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    z-index: 901;
    width: min(420px, 92vw);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 14px;
    padding: 1.5rem 1.6rem;
    box-shadow: 0 30px 60px rgba(0, 0, 0, 0.45);
    animation: pop-in 160ms ease-out;
  }

  h2 {
    margin: 0 0 1rem;
    font-size: 1.05rem;
  }

  label {
    display: block;
    margin-bottom: 0.85rem;
  }

  label span {
    display: block;
    font-size: 0.8rem;
    color: var(--fg-muted);
    margin-bottom: 0.3rem;
  }

  .hint {
    font-style: italic;
    opacity: 0.8;
  }

  input {
    width: 100%;
    padding: 0.55rem 0.7rem;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 7px;
    color: var(--fg);
    font-size: 0.92rem;
    font-family: inherit;
  }

  input:focus {
    outline: none;
    border-color: var(--accent);
  }

  code {
    background: var(--surface-2);
    padding: 0 0.3rem;
    border-radius: 3px;
    font-size: 0.78rem;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.55rem;
    margin-top: 0.4rem;
  }

  button {
    padding: 0.5rem 1rem;
    border-radius: 7px;
    cursor: pointer;
    font-size: 0.88rem;
    font-weight: 500;
    transition: filter 120ms ease;
  }

  button.ghost {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--fg);
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

  button.primary:hover:not(:disabled) {
    filter: brightness(1.1);
  }

  button.primary:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  @keyframes fade-in {
    from { opacity: 0; }
    to { opacity: 1; }
  }
  @keyframes pop-in {
    from {
      opacity: 0;
      transform: translate(-50%, -45%) scale(0.96);
    }
    to {
      opacity: 1;
      transform: translate(-50%, -50%) scale(1);
    }
  }
</style>
