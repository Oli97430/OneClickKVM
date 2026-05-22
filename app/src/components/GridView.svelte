<script lang="ts">
  import type { GridPeerView } from "../ipc";

  let { peers }: { peers: GridPeerView[] } = $props();

  // Calcule la bbox englobante de tous les pairs pour normaliser sur 0..1.
  const bounds = $derived(() => {
    if (peers.length === 0) {
      return { x: 0, y: 0, w: 1, h: 1 };
    }
    let min_x = Infinity, min_y = Infinity, max_x = -Infinity, max_y = -Infinity;
    for (const p of peers) {
      min_x = Math.min(min_x, p.bbox.x);
      min_y = Math.min(min_y, p.bbox.y);
      max_x = Math.max(max_x, p.bbox.x + p.bbox.w);
      max_y = Math.max(max_y, p.bbox.y + p.bbox.h);
    }
    return {
      x: min_x,
      y: min_y,
      w: Math.max(1, max_x - min_x),
      h: Math.max(1, max_y - min_y),
    };
  });

  function normalize(p: GridPeerView) {
    const b = bounds();
    return {
      left: ((p.bbox.x - b.x) / b.w) * 100,
      top: ((p.bbox.y - b.y) / b.h) * 100,
      width: (p.bbox.w / b.w) * 100,
      height: (p.bbox.h / b.h) * 100,
    };
  }
</script>

{#if peers.length === 0}
  <div class="empty">Aucune disposition.</div>
{:else}
  <div class="canvas">
    {#each peers as p (p.name + p.bbox.x)}
      {@const pos = normalize(p)}
      <div
        class="tile"
        class:self={p.is_self}
        style="left: {pos.left}%; top: {pos.top}%; width: {pos.width}%; height: {pos.height}%"
        title="{p.name} ({p.bbox.w}×{p.bbox.h})"
      >
        <div class="tile-label">
          <span class="name">{p.name}</span>
          {#if p.hotkey != null}
            <span class="hotkey">Ctrl+Alt+Win+{p.hotkey}</span>
          {/if}
        </div>
      </div>
    {/each}
  </div>
  <div class="legend">
    Glissez le curseur vers un bord pour basculer.
    Touche <kbd>0</kbd> du hotkey = retour local.
  </div>
{/if}

<style>
  .empty {
    color: var(--fg-muted);
    text-align: center;
    padding: 1rem;
    font-size: 0.85rem;
  }

  .canvas {
    position: relative;
    width: 100%;
    aspect-ratio: 3 / 1;
    background: var(--surface-2);
    border-radius: 8px;
    border: 1px dashed var(--border);
    overflow: hidden;
    min-height: 120px;
  }

  .tile {
    position: absolute;
    background: rgba(59, 130, 246, 0.18);
    border: 2px solid rgba(59, 130, 246, 0.6);
    border-radius: 4px;
    display: flex;
    align-items: center;
    justify-content: center;
    text-align: center;
    box-sizing: border-box;
    transition: background 120ms ease;
  }

  .tile:hover {
    background: rgba(59, 130, 246, 0.3);
  }

  .tile.self {
    background: rgba(34, 197, 94, 0.18);
    border-color: rgba(34, 197, 94, 0.7);
  }

  .tile-label {
    padding: 0.25rem 0.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    max-width: 100%;
  }

  .name {
    font-weight: 600;
    font-size: 0.78rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .hotkey {
    font-size: 0.6rem;
    color: var(--fg-muted);
    font-family: "Cascadia Code", "Consolas", monospace;
  }

  .legend {
    margin-top: 0.55rem;
    text-align: center;
    color: var(--fg-muted);
    font-size: 0.78rem;
  }

  kbd {
    background: var(--surface-2);
    padding: 0.05rem 0.4rem;
    border-radius: 4px;
    border: 1px solid var(--border);
    font-family: "Cascadia Code", "Consolas", monospace;
    font-size: 0.75rem;
  }
</style>
