<script lang="ts">
  import { fingerprintToString, unpairPeer, type PeerView } from "../ipc";
  import { openPairModal } from "./PairModal.svelte";
  import { pushNotification } from "./Notifications.svelte";
  import { t } from "../i18n.svelte";

  let { peers, onPeersChanged }: {
    peers: PeerView[];
    onPeersChanged?: () => void;
  } = $props();

  let confirmRemove = $state<string | null>(null);

  function startPair(peer: PeerView) {
    openPairModal(peer.last_addr ?? "");
  }

  function peerKey(p: PeerView): string {
    return p.device_id.join(",");
  }

  async function doUnpair(peer: PeerView) {
    try {
      await unpairPeer(peer.device_id);
      pushNotification({
        level: "info",
        title: "Pair retire",
        body: `${peer.name} a ete retire de vos pairs connus.`,
      });
      confirmRemove = null;
      onPeersChanged?.();
    } catch (e) {
      pushNotification({
        level: "error",
        title: "Echec",
        body: String(e),
      });
    }
  }
</script>

{#if peers.length === 0}
  <div class="empty">
    <div class="empty-icon">·</div>
    <p>{t("peer.empty")}</p>
    <p class="hint">{t("peer.empty_hint")}</p>
  </div>
{:else}
  <ul class="peers">
    {#each peers as peer (peer.device_id.join(","))}
      <li class="peer" class:online={peer.online}>
        <div class="left">
          <div class="dot" class:on={peer.online} class:paired={peer.paired}></div>
          <div class="meta">
            <div class="name-row">
              <span class="name">{peer.name}</span>
              {#if peer.paired}
                <span class="badge paired">{t("peer.badge_paired")}</span>
              {:else}
                <span class="badge unpaired">{t("peer.badge_unpaired")}</span>
              {/if}
              {#if peer.discovered}
                <span class="badge discovered">{t("peer.badge_discovered")}</span>
              {/if}
            </div>
            <div class="fp mono">{fingerprintToString(peer.fingerprint)}</div>
            {#if peer.last_addr}
              <div class="addr mono">{peer.last_addr}</div>
            {/if}
          </div>
        </div>
        <div class="right">
          {#if confirmRemove === peerKey(peer)}
            <button class="action" onclick={() => (confirmRemove = null)}>
              {t("peer.unpair_cancel")}
            </button>
            <button class="action danger" onclick={() => doUnpair(peer)}>
              {t("peer.unpair_confirm")}
            </button>
          {:else if peer.paired}
            <button class="action" title={t("peer.reconnect")} onclick={() => startPair(peer)}>
              {t("peer.reconnect")}
            </button>
            <button
              class="action"
              title={t("peer.remove_title")}
              onclick={() => (confirmRemove = peerKey(peer))}
            >
              ×
            </button>
          {:else}
            <button class="action primary" onclick={() => startPair(peer)}>
              {t("peer.pair")}
            </button>
          {/if}
        </div>
      </li>
    {/each}
  </ul>
{/if}

<style>
  .empty {
    text-align: center;
    padding: 2rem 1rem;
    color: var(--fg-muted);
  }

  .empty-icon {
    font-size: 2rem;
    line-height: 1;
    margin-bottom: 0.5rem;
    opacity: 0.5;
  }

  .empty p {
    margin: 0.25rem 0;
  }

  .empty .hint {
    font-size: 0.82rem;
    opacity: 0.8;
  }

  .peers {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
  }

  .peer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.85rem 1rem;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 10px;
    transition: border-color 120ms ease;
  }

  .peer:hover {
    border-color: rgba(59, 130, 246, 0.4);
  }

  .left {
    display: flex;
    align-items: flex-start;
    gap: 0.85rem;
    min-width: 0;
  }

  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--fg-muted);
    opacity: 0.4;
    margin-top: 0.35rem;
    flex-shrink: 0;
  }

  .dot.paired {
    opacity: 0.75;
  }

  .dot.on {
    background: var(--success);
    opacity: 1;
    box-shadow: 0 0 0 4px rgba(34, 197, 94, 0.15);
  }

  .meta {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
  }

  .name-row {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    flex-wrap: wrap;
  }

  .name {
    font-weight: 600;
    font-size: 0.95rem;
  }

  .badge {
    font-size: 0.68rem;
    padding: 0.1rem 0.45rem;
    border-radius: 4px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-weight: 600;
  }

  .badge.paired {
    background: rgba(34, 197, 94, 0.15);
    color: #86efac;
  }

  .badge.unpaired {
    background: rgba(245, 158, 11, 0.15);
    color: #fcd34d;
  }

  .badge.discovered {
    background: rgba(59, 130, 246, 0.15);
    color: #93c5fd;
  }

  .fp,
  .addr {
    font-family: "Cascadia Code", "JetBrains Mono", "Consolas", monospace;
    font-size: 0.72rem;
    color: var(--fg-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  button.action {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--fg);
    padding: 0.4rem 0.85rem;
    border-radius: 6px;
    cursor: pointer;
    font-size: 0.82rem;
    transition: background 120ms ease;
  }

  button.action:hover {
    background: var(--bg-hover);
  }

  button.action.primary {
    background: linear-gradient(135deg, #3b82f6 0%, #6366f1 100%);
    border: none;
    color: white;
    font-weight: 600;
  }

  button.action.primary:hover {
    filter: brightness(1.1);
  }

  button.action.danger {
    background: linear-gradient(135deg, #ef4444 0%, #b91c1c 100%);
    border: none;
    color: white;
    font-weight: 600;
  }

  button.action.danger:hover {
    filter: brightness(1.1);
  }
</style>
