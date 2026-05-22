<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { sendFiles, type PeerView } from "../ipc";
  import { pushNotification } from "./Notifications.svelte";

  let { peers }: { peers: PeerView[] } = $props();

  // Pair cible : par defaut le premier pair online + paired.
  let selectedDeviceId = $state<string>("");
  let isHovering = $state(false);
  let unlisten: (() => void) | null = null;

  const eligiblePeers = $derived(
    peers.filter((p) => p.paired && p.online),
  );

  // Defaut : premier pair eligible.
  $effect(() => {
    if (!selectedDeviceId && eligiblePeers.length > 0) {
      selectedDeviceId = peerKey(eligiblePeers[0]);
    }
  });

  function peerKey(p: PeerView): string {
    return p.device_id.join(",");
  }

  function findPeerByKey(key: string): PeerView | undefined {
    return peers.find((p) => peerKey(p) === key);
  }

  onMount(async () => {
    try {
      const win = getCurrentWindow();
      unlisten = await win.onDragDropEvent(async (event) => {
        if (event.payload.type === "over") {
          isHovering = true;
        } else if (event.payload.type === "leave") {
          isHovering = false;
        } else if (event.payload.type === "drop") {
          isHovering = false;
          const paths = event.payload.paths;
          if (paths.length === 0) return;
          const peer = findPeerByKey(selectedDeviceId);
          if (!peer) {
            pushNotification({
              level: "warn",
              title: "Pas de pair selectionne",
              body: "Connecte un pair avant de deposer des fichiers.",
            });
            return;
          }
          try {
            await sendFiles(peer.device_id, paths);
            pushNotification({
              level: "info",
              title: "Envoi en cours",
              body: `${paths.length} fichier(s) vers ${peer.name}`,
            });
          } catch (e) {
            pushNotification({
              level: "error",
              title: "Echec envoi",
              body: String(e),
            });
          }
        }
      });
    } catch (e) {
      console.error("DropZone onDragDropEvent failed", e);
    }
  });

  onDestroy(() => {
    unlisten?.();
  });
</script>

<div class="drop-zone" class:hovering={isHovering} class:disabled={eligiblePeers.length === 0}>
  <div class="icon">⇩</div>
  <div class="text">
    {#if eligiblePeers.length === 0}
      <div class="title">Connecte un pair pour activer le transfert</div>
      <div class="hint">La zone s'illuminera des qu'un pair est en ligne.</div>
    {:else}
      <div class="title">Glisse des fichiers ici pour les envoyer</div>
      <div class="select-row">
        <label for="peer-select">Cible :</label>
        <select id="peer-select" bind:value={selectedDeviceId}>
          {#each eligiblePeers as p (peerKey(p))}
            <option value={peerKey(p)}>{p.name}</option>
          {/each}
        </select>
      </div>
    {/if}
  </div>
</div>

<style>
  .drop-zone {
    border: 2px dashed var(--border);
    border-radius: 12px;
    padding: 1.25rem 1.5rem;
    display: flex;
    align-items: center;
    gap: 1rem;
    background: var(--surface-2);
    transition: border-color 150ms ease, background 150ms ease, transform 100ms ease;
  }

  .drop-zone.hovering {
    border-color: var(--accent);
    background: rgba(59, 130, 246, 0.12);
    transform: scale(1.005);
  }

  .drop-zone.disabled {
    opacity: 0.65;
  }

  .icon {
    font-size: 2.5rem;
    line-height: 1;
    color: var(--accent);
  }

  .drop-zone.disabled .icon {
    color: var(--fg-muted);
  }

  .text {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .title {
    font-weight: 600;
    font-size: 0.95rem;
  }

  .hint {
    font-size: 0.8rem;
    color: var(--fg-muted);
  }

  .select-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.85rem;
  }

  label {
    color: var(--fg-muted);
  }

  select {
    background: var(--surface);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.25rem 0.5rem;
    font-size: 0.85rem;
    font-family: inherit;
    cursor: pointer;
  }

  select:focus {
    outline: none;
    border-color: var(--accent);
  }
</style>
