<script lang="ts" module>
  import type { TransferProgressView } from "../ipc";

  // Store module-level partage : on stocke les transferts par ID.
  let _transfers = $state<Map<string, TransferProgressView>>(new Map());

  export function applyTransferProgress(p: TransferProgressView) {
    const next = new Map(_transfers);
    next.set(p.transfer_id, p);
    // Nettoie les transferts termines depuis plus de 8s.
    setTimeout(() => {
      if (p.state === "done" || p.state === "error" || p.state === "cancelled") {
        const m = new Map(_transfers);
        m.delete(p.transfer_id);
        _transfers = m;
      }
    }, 8000);
    _transfers = next;
  }

  export function getTransfers() {
    return _transfers;
  }
</script>

<script lang="ts">
  const transfers = $derived(Array.from(getTransfers().values()));

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} o`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} Ko`;
    if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} Mo`;
    return `${(n / (1024 * 1024 * 1024)).toFixed(2)} Go`;
  }

  function pct(p: TransferProgressView): number {
    if (p.bytes_total === 0) return 0;
    return Math.min(100, (p.bytes_done / p.bytes_total) * 100);
  }
</script>

{#if transfers.length > 0}
  <ul class="transfers">
    {#each transfers as t (t.transfer_id)}
      <li class="transfer {t.state}">
        <div class="row">
          <span class="dir">{t.direction === "outbound" ? "→" : "←"}</span>
          <span class="peer">{t.peer_name || "(local)"}</span>
          <span class="file mono">{t.current_file}</span>
          <span class="bytes">
            {fmtBytes(t.bytes_done)} / {fmtBytes(t.bytes_total)}
          </span>
        </div>
        <div class="bar-track">
          <div class="bar-fill" style="width: {pct(t)}%"></div>
        </div>
        {#if t.state === "error"}
          <div class="error-msg">⚠ {t.error}</div>
        {:else if t.state === "done"}
          <div class="done-msg">✓ Termine</div>
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .transfers {
    list-style: none;
    margin: 0.85rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
  }

  .transfer {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0.55rem 0.7rem;
    font-size: 0.82rem;
  }

  .transfer.done {
    border-color: rgba(34, 197, 94, 0.4);
  }

  .transfer.error {
    border-color: rgba(239, 68, 68, 0.4);
  }

  .row {
    display: flex;
    gap: 0.55rem;
    align-items: center;
  }

  .dir {
    font-weight: 700;
    font-size: 0.95rem;
    color: var(--accent);
  }

  .peer {
    font-weight: 600;
  }

  .file {
    flex: 1;
    color: var(--fg-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.75rem;
  }

  .mono {
    font-family: "Cascadia Code", "Consolas", monospace;
  }

  .bytes {
    font-variant-numeric: tabular-nums;
    font-size: 0.75rem;
    color: var(--fg-muted);
  }

  .bar-track {
    margin-top: 0.4rem;
    height: 4px;
    background: rgba(255, 255, 255, 0.08);
    border-radius: 2px;
    overflow: hidden;
  }

  .bar-fill {
    height: 100%;
    background: linear-gradient(90deg, var(--accent) 0%, #8b5cf6 100%);
    transition: width 200ms ease-out;
  }

  .transfer.done .bar-fill {
    background: var(--success);
  }

  .transfer.error .bar-fill {
    background: var(--error);
  }

  .error-msg {
    margin-top: 0.3rem;
    font-size: 0.75rem;
    color: #fca5a5;
  }

  .done-msg {
    margin-top: 0.3rem;
    font-size: 0.75rem;
    color: #86efac;
  }
</style>
