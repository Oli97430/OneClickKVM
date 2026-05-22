<script lang="ts">
  import { fingerprintToString, type AppStatus } from "../ipc";
  import { openPairModal } from "./PairModal.svelte";

  let { status }: { status: AppStatus | null } = $props();
</script>

<div class="welcome">
  <div class="hero">
    <div class="emoji">👋</div>
    <h2>Bienvenue dans OneClick KVM</h2>
    <p class="subtitle">
      Partagez clavier, souris, audio et fichiers entre plusieurs PCs Windows,
      tout chiffre AES-256.
    </p>
  </div>

  {#if status}
    <div class="fingerprint-block">
      <span class="label">Empreinte de ce PC</span>
      <code class="fp">{fingerprintToString(status.self_fingerprint)}</code>
      <span class="hint">
        Communiquez-la a votre voisin pour qu'il sache que c'est bien vous.
      </span>
    </div>
  {/if}

  <div class="steps">
    <div class="step">
      <div class="num">1</div>
      <div>
        <strong>Demarrez le service</strong>
        <p>
          Cliquez "{status?.listening ? "✅ En ecoute" : "Hors ligne → En ecoute"}"
          en haut. mDNS et le broadcast UDP commenceront a annoncer ce PC.
        </p>
      </div>
    </div>
    <div class="step">
      <div class="num">2</div>
      <div>
        <strong>Lancez OneClick KVM sur un autre PC du meme reseau</strong>
        <p>
          Faites la meme chose la-bas. Les deux instances vont se decouvrir
          automatiquement via mDNS.
        </p>
      </div>
    </div>
    <div class="step">
      <div class="num">3</div>
      <div>
        <strong>Appairez</strong>
        <p>
          Si la decouverte echoue, cliquez le bouton ci-dessous pour saisir
          manuellement l'adresse <code>ip:port</code> du pair.
        </p>
        <button class="primary" onclick={() => openPairModal()}>
          Appairer manuellement
        </button>
      </div>
    </div>
  </div>
</div>

<style>
  .welcome {
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
  }

  .hero {
    text-align: center;
    padding: 1rem 0 0.5rem;
  }

  .emoji {
    font-size: 2.5rem;
    line-height: 1;
    margin-bottom: 0.4rem;
  }

  h2 {
    margin: 0 0 0.4rem;
    font-size: 1.3rem;
  }

  .subtitle {
    margin: 0;
    color: var(--fg-muted);
    font-size: 0.92rem;
    line-height: 1.5;
    max-width: 520px;
    margin: 0 auto;
  }

  .fingerprint-block {
    text-align: center;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 0.85rem 1rem;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.4rem;
  }

  .label {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-muted);
  }

  .fp {
    font-family: "Cascadia Code", "JetBrains Mono", "Consolas", monospace;
    font-size: 0.95rem;
    color: var(--accent);
    letter-spacing: 0.04em;
  }

  .hint {
    font-size: 0.78rem;
    color: var(--fg-muted);
  }

  .steps {
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
  }

  .step {
    display: flex;
    align-items: flex-start;
    gap: 0.85rem;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 0.85rem 1rem;
  }

  .num {
    flex-shrink: 0;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    background: linear-gradient(135deg, #3b82f6 0%, #8b5cf6 100%);
    color: white;
    display: grid;
    place-items: center;
    font-weight: 700;
    font-size: 0.9rem;
  }

  .step strong {
    display: block;
    font-size: 0.95rem;
    margin-bottom: 0.2rem;
  }

  .step p {
    margin: 0 0 0.5rem;
    color: var(--fg-muted);
    font-size: 0.85rem;
    line-height: 1.5;
  }

  .step code {
    background: var(--surface);
    padding: 0 0.3rem;
    border-radius: 3px;
    font-family: "Cascadia Code", "Consolas", monospace;
    font-size: 0.78rem;
  }

  button.primary {
    margin-top: 0.4rem;
    background: linear-gradient(135deg, #3b82f6 0%, #6366f1 100%);
    border: none;
    color: white;
    padding: 0.5rem 1rem;
    border-radius: 7px;
    cursor: pointer;
    font-weight: 600;
    font-size: 0.88rem;
    font-family: inherit;
  }

  button.primary:hover {
    filter: brightness(1.1);
  }
</style>
