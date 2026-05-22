<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import {
    getAppStatus,
    listPeers,
    onBackendEvent,
    type AppStatus,
    type PeerView,
    type BackendEvent,
  } from "./ipc";
  import StatusBar from "./components/StatusBar.svelte";
  import PeerList from "./components/PeerList.svelte";
  import GridView from "./components/GridView.svelte";
  import DropZone from "./components/DropZone.svelte";
  import VideoView from "./components/VideoView.svelte";
  import TransferList, {
    applyTransferProgress,
  } from "./components/TransferList.svelte";
  import Notifications, {
    pushNotification,
  } from "./components/Notifications.svelte";
  import PairModal, { openPairModal } from "./components/PairModal.svelte";
  import SettingsView from "./components/SettingsView.svelte";
  import AboutView from "./components/AboutView.svelte";
  import WelcomeCard from "./components/WelcomeCard.svelte";
  import PairingBanner from "./components/PairingBanner.svelte";
  import { getAppConfig, getGrid, getInboxDir, type GridPeerView } from "./ipc";
  import { setLang, t } from "./i18n.svelte";
  import { setTheme } from "./theme.svelte";

  type Tab = "home" | "settings" | "about";
  let activeTab = $state<Tab>("home");

  let status = $state<AppStatus | null>(null);
  let peers = $state<PeerView[]>([]);
  let grid = $state<GridPeerView[]>([]);
  let inboxDir = $state<string>("");
  let error = $state<string | null>(null);
  let unlisten: (() => void) | null = null;

  async function refresh() {
    try {
      status = await getAppStatus();
      peers = await listPeers();
      grid = await getGrid();
      error = null;
    } catch (e) {
      error = String(e);
    }
  }

  onMount(async () => {
    // Charge la langue et le theme depuis AppConfig.
    try {
      const cfg = await getAppConfig();
      setLang(cfg.language);
      setTheme(cfg.theme);
    } catch {
      // pas grave, fallback
    }
    await refresh();
    try {
      inboxDir = await getInboxDir();
    } catch (e) {
      console.warn("getInboxDir failed", e);
    }
    unlisten = await onBackendEvent(handleEvent);
  });

  onDestroy(() => {
    unlisten?.();
  });

  function handleEvent(ev: BackendEvent) {
    switch (ev.type) {
      case "status_changed":
        status = ev.status;
        break;
      case "peer_discovered":
        peers = [...peers.filter((p) => !sameId(p.device_id, ev.peer.device_id)), ev.peer];
        break;
      case "peer_connected":
        peers = peers.map((p) =>
          sameId(p.device_id, ev.device_id) ? { ...p, online: true, paired: true } : p,
        );
        // Rafraichit la grille apres une connexion pour faire apparaitre le pair.
        refresh();
        break;
      case "peer_disconnected":
        peers = peers.map((p) =>
          sameId(p.device_id, ev.device_id) ? { ...p, online: false } : p,
        );
        refresh();
        pushNotification({
          level: "warn",
          title: "Pair deconnecte",
          body: ev.reason,
        });
        break;
      case "notification":
        pushNotification({ level: ev.level, title: ev.title, body: ev.body });
        break;
      case "confirmation_requested":
        // TODO : modale de confirmation
        pushNotification({
          level: "info",
          title: "Confirmation requise",
          body: ev.prompt,
        });
        break;
      case "transfer_progress":
        applyTransferProgress(ev.progress);
        break;
    }
  }

  function sameId(a: number[], b: number[]) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
    return true;
  }
</script>

<main>
  <header>
    <div class="brand">
      <div class="logo">OK</div>
      <div class="title-block">
        <h1>OneClick KVM</h1>
        <span class="subtitle">Controle multi-PC chiffre AES-256</span>
      </div>
    </div>
    <div class="header-actions">
      <nav class="tabs">
        <button
          class="tab"
          class:active={activeTab === "home"}
          onclick={() => (activeTab = "home")}
        >
          {t("tab.home")}
        </button>
        <button
          class="tab"
          class:active={activeTab === "settings"}
          onclick={() => (activeTab = "settings")}
        >
          {t("tab.settings")}
        </button>
        <button
          class="tab"
          class:active={activeTab === "about"}
          onclick={() => (activeTab = "about")}
        >
          {t("tab.about")}
        </button>
      </nav>
      {#if activeTab === "home"}
        <button class="ghost" onclick={() => openPairModal()}>{t("header.pair")}</button>
        <button class="ghost" onclick={refresh}>{t("header.refresh")}</button>
      {/if}
    </div>
  </header>

  {#if error}
    <div class="banner banner-error">
      <strong>Erreur backend :</strong> {error}
    </div>
  {/if}

  {#if activeTab === "home"}
    <StatusBar {status} />
    <PairingBanner />

    {#if peers.length === 0}
      <section class="welcome-section">
        <WelcomeCard {status} />
      </section>
    {:else}
      <section>
        <h2>{t("section.layout")}</h2>
        <GridView peers={grid} />
      </section>

      <section>
        <h2>{t("section.peers")} ({peers.length})</h2>
        <PeerList {peers} onPeersChanged={refresh} />
      </section>

      <section>
        <h2>Ecrans partages</h2>
        <VideoView {peers} />
      </section>

      <section>
        <h2>{t("section.transfers")}</h2>
        <DropZone {peers} />
        <TransferList />
        {#if inboxDir}
          <div class="inbox-info">
            {t("inbox.label")} <code>{inboxDir}</code>
          </div>
        {/if}
      </section>
    {/if}
  {:else if activeTab === "settings"}
    <section>
      <h2>Parametres</h2>
      <SettingsView />
    </section>
  {:else if activeTab === "about"}
    <section>
      <h2>A propos</h2>
      <AboutView />
    </section>
  {/if}

  <footer>
    <span>v0.1.0 · phase 1 (squelette)</span>
    <span class="muted">·</span>
    <span class="muted">Windows · Rust · Tauri 2 · Svelte 5</span>
  </footer>

  <PairModal />
  <Notifications />
</main>

<style>
  main {
    max-width: 980px;
    margin: 0 auto;
    padding: 1.5rem 1.75rem 3rem;
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.5rem;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 0.85rem;
  }

  .logo {
    width: 42px;
    height: 42px;
    border-radius: 10px;
    background: linear-gradient(135deg, #3b82f6 0%, #8b5cf6 100%);
    color: white;
    font-weight: 700;
    display: grid;
    place-items: center;
    font-size: 0.95rem;
    letter-spacing: 0.02em;
    box-shadow: 0 4px 14px rgba(59, 130, 246, 0.35);
  }

  h1 {
    margin: 0;
    font-size: 1.25rem;
    letter-spacing: -0.01em;
  }

  .subtitle {
    font-size: 0.78rem;
    color: var(--fg-muted);
  }

  button.ghost {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--fg);
    padding: 0.4rem 0.9rem;
    border-radius: 8px;
    cursor: pointer;
    font-size: 0.85rem;
    transition: background 120ms ease;
  }

  button.ghost:hover {
    background: var(--bg-hover);
  }

  .header-actions {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }

  .tabs {
    display: flex;
    gap: 0.15rem;
    background: var(--surface-2);
    padding: 0.2rem;
    border-radius: 9px;
    margin-right: 0.4rem;
  }

  .tab {
    background: transparent;
    border: none;
    color: var(--fg-muted);
    padding: 0.4rem 0.8rem;
    border-radius: 7px;
    cursor: pointer;
    font-size: 0.84rem;
    font-weight: 500;
    transition: background 120ms ease, color 120ms ease;
  }

  .tab:hover {
    color: var(--fg);
  }

  .tab.active {
    background: var(--surface);
    color: var(--fg);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
  }

  .inbox-info {
    margin-top: 0.6rem;
    text-align: center;
    font-size: 0.78rem;
    color: var(--fg-muted);
  }

  .inbox-info code {
    background: var(--surface-2);
    padding: 0.05rem 0.4rem;
    border-radius: 4px;
    font-family: "Cascadia Code", "Consolas", monospace;
  }

  .banner {
    padding: 0.75rem 1rem;
    border-radius: 8px;
    font-size: 0.88rem;
  }

  .banner-error {
    background: rgba(239, 68, 68, 0.12);
    border: 1px solid rgba(239, 68, 68, 0.35);
    color: #fca5a5;
  }

  section {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 1.25rem 1.4rem;
  }

  section h2 {
    margin: 0 0 0.85rem;
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--fg-muted);
    letter-spacing: 0.02em;
    text-transform: uppercase;
  }

  section.welcome-section {
    padding: 1.75rem 1.75rem;
  }

  footer {
    display: flex;
    gap: 0.4rem;
    font-size: 0.75rem;
    color: var(--fg-muted);
    justify-content: center;
    margin-top: 0.75rem;
  }

  .muted {
    color: var(--fg-muted);
  }
</style>
