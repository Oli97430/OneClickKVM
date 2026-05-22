<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import {
    onVideoFrame,
    onVideoStart,
    onVideoStop,
    type VideoFrameEvent,
    type VideoStartEvent,
    type PeerView,
  } from "../ipc";

  let { peers }: { peers: PeerView[] } = $props();

  interface RemoteScreen {
    deviceId: number[];
    width: number;
    height: number;
    fps: number;
    lastSeq: number;
    dataUrl: string;
    fpsActual: number;
    frameCount: number;
    lastSecond: number;
  }

  let screens = $state<Map<string, RemoteScreen>>(new Map());
  let unlistens: Array<() => void> = [];

  function keyOf(id: number[]): string {
    return id.join(",");
  }

  function peerName(id: number[]): string {
    const k = keyOf(id);
    return peers.find((p) => keyOf(p.device_id) === k)?.name ?? "Pair inconnu";
  }

  onMount(async () => {
    const u1 = await onVideoStart((e: VideoStartEvent) => {
      const k = keyOf(e.device_id);
      const m = new Map(screens);
      m.set(k, {
        deviceId: e.device_id,
        width: e.width,
        height: e.height,
        fps: e.fps,
        lastSeq: 0,
        dataUrl: "",
        fpsActual: 0,
        frameCount: 0,
        lastSecond: Date.now(),
      });
      screens = m;
    });
    const u2 = await onVideoFrame((e: VideoFrameEvent) => {
      const k = keyOf(e.device_id);
      const m = new Map(screens);
      const prev = m.get(k) ?? {
        deviceId: e.device_id,
        width: 0,
        height: 0,
        fps: 0,
        lastSeq: 0,
        dataUrl: "",
        fpsActual: 0,
        frameCount: 0,
        lastSecond: Date.now(),
      };
      const now = Date.now();
      let frameCount = prev.frameCount + 1;
      let lastSecond = prev.lastSecond;
      let fpsActual = prev.fpsActual;
      if (now - lastSecond >= 1000) {
        fpsActual = frameCount;
        frameCount = 0;
        lastSecond = now;
      }
      m.set(k, {
        ...prev,
        lastSeq: e.seq,
        dataUrl: `data:image/jpeg;base64,${e.jpeg_b64}`,
        frameCount,
        lastSecond,
        fpsActual,
      });
      screens = m;
    });
    const u3 = await onVideoStop((e: { device_id: number[] }) => {
      const k = keyOf(e.device_id);
      const m = new Map(screens);
      m.delete(k);
      screens = m;
    });
    unlistens = [u1, u2, u3];
  });

  onDestroy(() => {
    unlistens.forEach((u) => u());
  });

  const screensArray = $derived(Array.from(screens.values()));
</script>

{#if screensArray.length === 0}
  <div class="empty">
    <span>Aucun ecran partage pour l'instant.</span>
    <span class="hint">Un pair doit cliquer "Partager ecran" pour que sa capture apparaisse ici.</span>
  </div>
{:else}
  <div class="grid" class:single={screensArray.length === 1} class:multi={screensArray.length > 1}>
    {#each screensArray as s (keyOf(s.deviceId))}
      <div class="tile">
        <div class="tile-header">
          <span class="name">{peerName(s.deviceId)}</span>
          <span class="meta">
            {s.width}×{s.height}
            {#if s.fpsActual > 0}· {s.fpsActual} fps{/if}
          </span>
        </div>
        {#if s.dataUrl}
          <img src={s.dataUrl} alt="Capture de {peerName(s.deviceId)}" />
        {:else}
          <div class="loading">Reception de la premiere frame...</div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .empty {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    text-align: center;
    padding: 2rem 1rem;
    color: var(--fg-muted);
    font-size: 0.88rem;
  }

  .empty .hint {
    opacity: 0.75;
    font-size: 0.78rem;
  }

  .grid {
    display: grid;
    gap: 0.85rem;
  }

  .grid.single {
    grid-template-columns: 1fr;
  }

  .grid.multi {
    grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  }

  .tile {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 10px;
    overflow: hidden;
  }

  .tile-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem 0.75rem;
    background: rgba(0, 0, 0, 0.2);
    font-size: 0.8rem;
  }

  .name {
    font-weight: 600;
  }

  .meta {
    color: var(--fg-muted);
    font-family: "Cascadia Code", "Consolas", monospace;
    font-size: 0.72rem;
  }

  img {
    display: block;
    width: 100%;
    height: auto;
    background: #000;
  }

  .loading {
    aspect-ratio: 16 / 9;
    display: grid;
    place-items: center;
    color: var(--fg-muted);
    background: #000;
    font-size: 0.8rem;
  }
</style>
