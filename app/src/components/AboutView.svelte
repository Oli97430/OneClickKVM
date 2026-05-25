<script lang="ts">
  import { onMount } from "svelte";
  import { fingerprintToString, getAboutInfo, type AboutInfo } from "../ipc";
  import { pushNotification } from "./Notifications.svelte";
  import { t } from "../i18n.svelte";

  let info = $state<AboutInfo | null>(null);
  let copied = $state(false);
  let mounted = $state(false);

  onMount(async () => {
    try {
      info = await getAboutInfo();
    } catch (e) {
      pushNotification({
        level: "error",
        title: "Erreur",
        body: String(e),
      });
    }
    // Petit delay pour que les animations CSS partent en cascade.
    requestAnimationFrame(() => (mounted = true));
  });

  async function copyFingerprint() {
    if (!info) return;
    const fp = fingerprintToString(info.self_fingerprint);
    try {
      await navigator.clipboard.writeText(fp);
      copied = true;
      pushNotification({
        level: "success",
        title: "Empreinte copiée",
        body: fp,
      });
      setTimeout(() => (copied = false), 1800);
    } catch (e) {
      pushNotification({
        level: "warn",
        title: "Copie impossible",
        body: String(e),
      });
    }
  }

  // Format un encoder name en quelque chose de plus calme : strip emoji, simplifier
  function calmEncoderLabel(raw: string): string {
    return raw
      .replace(/[🚀⚡✨🎉]/gu, "")
      .replace(/—\s*vrai GPU encoding/gi, "")
      .trim();
  }
</script>

<div class="washi" class:mounted>
  {#if !info}
    <div class="awaiting">
      <span class="ink-dot"></span>
      <span>Lecture du système…</span>
    </div>
  {:else}
    <!-- ─── FRONTISPIECE ────────────────────────────────────────── -->
    <section class="frontispiece">
      <div class="kana" aria-hidden="true">識</div>
      <h1 class="brand">
        <span class="brand-line-1">OneClick</span>
        <span class="brand-line-2">KVM</span>
      </h1>
      <div class="version-line">
        <span class="rule"></span>
        <span class="version-num">{info.version}</span>
        <span class="rule"></span>
      </div>
    </section>

    <!-- ─── IDENTITY CARD ───────────────────────────────────────── -->
    <section class="identity">
      <h2 class="section-title">Identité de cette machine</h2>
      <p class="section-sub">
        Empreinte <em>Ed25519</em>. À partager à l'oral ou par signal
        de confiance pour authentifier un pair —
        <em>style WireGuard, style SSH</em>.
      </p>

      <div class="card">
        <div class="card-corner top-left"></div>
        <div class="card-corner top-right"></div>
        <div class="card-corner bottom-left"></div>
        <div class="card-corner bottom-right"></div>

        <div class="card-row">
          <span class="card-label">FINGERPRINT</span>
          <button
            class="copy-btn"
            class:copied
            onclick={copyFingerprint}
            title="Copier l'empreinte"
          >
            {copied ? "✓ COPIÉ" : "COPIER"}
          </button>
        </div>

        <div class="fingerprint">
          {fingerprintToString(info.self_fingerprint)}
        </div>

        <div class="card-meta">
          <div>
            <span class="meta-label">HOST</span>
            <span class="meta-value">{info.self_hostname}</span>
          </div>
          <div>
            <span class="meta-label">PORT</span>
            <span class="meta-value">{info.tcp_port}</span>
          </div>
        </div>
      </div>
    </section>

    <!-- ─── ENCODING — SCREEN SHARE STATUS ──────────────────────── -->
    <section class="encoding">
      <h2 class="section-title">Encodage vidéo</h2>
      <p class="section-sub">
        Le pipeline H.264 actuellement choisi par l'application pour le partage
        d'écran. Sélection priorité <em>hardware async</em> &gt;
        <em>hardware sync</em> &gt; <em>software</em>.
      </p>

      <div class="encoder-active">
        <span class="dot"></span>
        <span>{calmEncoderLabel(info.mft_backend_active)}</span>
      </div>

      {#if info.h264_encoders.length > 0}
        <details class="encoders-detail">
          <summary>
            <span class="summary-arrow">▾</span>
            <span>{info.h264_encoders.length} encodeurs détectés sur ce système</span>
          </summary>
          <ul>
            {#each info.h264_encoders as enc}
              <li>
                <span class="tag-hw" class:on={enc.is_hardware}>
                  {enc.is_hardware ? "HW" : "SW"}
                </span>
                <span class="tag-mode" class:async={enc.is_async_mode}>
                  {enc.is_async_mode ? "async" : "sync"}
                </span>
                <span class="enc-name">{enc.friendly_name}</span>
              </li>
            {/each}
          </ul>
          <p class="hint">
            Depuis <em>V3.3.1</em>, les MFTs hardware async (NVENC, AMF, QuickSync
            récents) sont utilisables — un worker thread dédié gère l'event loop
            via <code>IMFAsyncCallback</code>. Avant, seul le sync (Microsoft AVC DX12)
            était sélectionnable.
          </p>
        </details>
      {/if}
    </section>

    <!-- ─── PROTOCOL — CRYPTOGRAPHIC MANIFESTO ──────────────────── -->
    <section class="manifesto">
      <h2 class="section-title">Protocole</h2>
      <div class="manifesto-body">
        <p>
          Tout octet sur le fil est chiffré <em>AES-256-GCM</em>. Les clés de
          session sont fraîches à chaque connexion via <em>X25519 ECDH</em> —
          <strong>Perfect Forward Secrecy</strong> : compromettre une clé long-terme
          ne déchiffre pas le passé.
        </p>
        <p>
          L'identité long-terme <em>Ed25519</em> de cette machine, dont
          l'empreinte est affichée ci-dessus, signe le transcript de
          l'handshake. Elle est scellée au repos par <em>Windows DPAPI</em>.
        </p>
        <p>
          Les fichiers reçus sont vérifiés <em>BLAKE3</em> avant d'atterrir
          dans l'inbox. L'audio et la vidéo voyagent en UDP avec
          <em>Reed-Solomon FEC k=4 m=2</em> — résilient à 2 paquets perdus
          par groupe.
        </p>
      </div>
    </section>

    <!-- ─── SHORTCUTS — KEYBOARD INK STAMPS ─────────────────────── -->
    <section class="shortcuts">
      <h2 class="section-title">{t("about.shortcuts.title")}</h2>
      <dl class="shortcut-list">
        <dt class="combo">
          <kbd>Ctrl</kbd><span>+</span><kbd>Alt</kbd><span>+</span><kbd>Win</kbd><span>+</span><kbd>0</kbd>
        </dt>
        <dd>{t("about.shortcuts.return")}</dd>

        <dt class="combo">
          <kbd>Ctrl</kbd><span>+</span><kbd>Alt</kbd><span>+</span><kbd>Win</kbd><span>+</span><kbd>1</kbd><span>…</span><kbd>9</kbd>
        </dt>
        <dd>{t("about.shortcuts.target_n")}</dd>

        <dt class="combo edge">
          <span class="edge-glyph">⇆</span>
        </dt>
        <dd>{t("about.shortcuts.edge")}</dd>
      </dl>
    </section>

    <!-- ─── COLOPHON ────────────────────────────────────────────── -->
    <footer class="colophon">
      <div class="col">
        <span class="col-label">TARGET</span>
        <span class="col-value mono">{info.rust_target}</span>
      </div>
      <div class="col">
        <span class="col-label">INBOX</span>
        <span class="col-value mono path">{info.inbox_dir}</span>
      </div>
      <div class="col">
        <span class="col-label">LICENCE</span>
        <span class="col-value">{info.license}</span>
      </div>
    </footer>

    <div class="seal" aria-hidden="true">
      <div class="seal-inner">
        <span>O</span><span>N</span><span>E</span>
        <span>C</span><span>L</span><span>I</span><span>C</span><span>K</span>
      </div>
    </div>
  {/if}
</div>

<style>
  /* ─── DESIGN TOKENS — washi paper + sumi ink ───────────────── */
  .washi {
    /* Force la palette japonaise indépendamment du thème app */
    --paper: #f5f1e8;
    --paper-deep: #ede7d8;
    --ink: #1c1b18;
    --ink-soft: #3e3a32;
    --ink-mute: #7d7768;
    --ink-faint: #b6ad99;
    --hairline: #d5cfc1;
    --indigo: #2a4858;
    --vermilion: #a04d2a;

    --serif-display: "Marcellus", "Cormorant Garamond", "Times New Roman", serif;
    --serif-body: "Spectral", "Source Serif Pro", Georgia, serif;
    --mono: "DM Mono", ui-monospace, "SF Mono", monospace;

    background: var(--paper);
    color: var(--ink);
    padding: clamp(2rem, 6vw, 5rem) clamp(1.5rem, 5vw, 4rem);
    max-width: 720px;
    margin: 0 auto;
    font-family: var(--serif-body);
    font-size: 15px;
    line-height: 1.6;
    position: relative;

    /* Subtle paper grain — moins fort que la landing */
    background-image:
      radial-gradient(ellipse at 20% 30%, rgba(160, 77, 42, 0.025) 0%, transparent 50%),
      radial-gradient(ellipse at 80% 70%, rgba(42, 72, 88, 0.02) 0%, transparent 50%),
      url("data:image/svg+xml,%3Csvg viewBox='0 0 200 200' xmlns='http://www.w3.org/2000/svg'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.85' numOctaves='2' stitchTiles='stitch'/%3E%3CfeColorMatrix values='0 0 0 0 0.11  0 0 0 0 0.11  0 0 0 0 0.10  0 0 0 0.025 0'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E");
  }

  /* Les fonts (Marcellus, Spectral, DM Mono) sont chargées via
     <link rel="stylesheet"> dans app/index.html pour éviter un waterfall
     de chargement bloquant. Pas de @import ici. */

  /* ─── LOADING / AWAITING ──────────────────────────────────── */
  .awaiting {
    text-align: center;
    padding: 4rem 0;
    color: var(--ink-mute);
    font-style: italic;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.8rem;
  }
  .ink-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--vermilion);
    animation: ink-pulse 1.8s ease-in-out infinite;
  }
  @keyframes ink-pulse {
    0%,
    100% {
      opacity: 1;
      transform: scale(1);
    }
    50% {
      opacity: 0.3;
      transform: scale(0.6);
    }
  }

  /* ─── FRONTISPIECE — hero quiet et cérémoniel ─────────────── */
  .frontispiece {
    text-align: center;
    padding: 1rem 0 3rem;
    position: relative;
  }

  .kana {
    font-family: var(--serif-display);
    font-size: clamp(60px, 12vw, 100px);
    color: var(--vermilion);
    opacity: 0;
    transform: scale(0.85);
    transition: opacity 1.4s ease-out, transform 1.4s ease-out;
    transition-delay: 0.1s;
    line-height: 1;
    margin-bottom: 1.5rem;
    /* Kanji 識 = "connaissance / discernement" — pour identifier */
  }
  .mounted .kana {
    opacity: 0.2;
    transform: scale(1);
  }

  h1.brand {
    font-family: var(--serif-display);
    font-weight: 400;
    font-size: clamp(38px, 5.5vw, 56px);
    letter-spacing: 0.04em;
    line-height: 1;
    margin: 0;
    color: var(--ink);
  }
  h1.brand .brand-line-1,
  h1.brand .brand-line-2 {
    display: inline-block;
    opacity: 0;
    transform: translateY(8px);
    transition: opacity 0.9s ease-out, transform 0.9s ease-out;
  }
  h1.brand .brand-line-1 {
    transition-delay: 0.4s;
    margin-right: 0.25em;
  }
  h1.brand .brand-line-2 {
    transition-delay: 0.55s;
    font-style: italic;
    color: var(--indigo);
  }
  .mounted h1.brand .brand-line-1,
  .mounted h1.brand .brand-line-2 {
    opacity: 1;
    transform: translateY(0);
  }

  .version-line {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 14px;
    margin-top: 1.5rem;
    opacity: 0;
    transition: opacity 0.8s ease-out;
    transition-delay: 0.9s;
  }
  .mounted .version-line {
    opacity: 1;
  }
  .version-line .rule {
    height: 1px;
    width: 40px;
    background: var(--ink-faint);
  }
  .version-line .version-num {
    font-family: var(--mono);
    font-weight: 300;
    font-size: 12px;
    letter-spacing: 0.18em;
    color: var(--ink-mute);
  }

  /* ─── SECTIONS — vertical rhythm, hairlines au lieu de cards ── */
  section {
    padding: 3rem 0;
    border-bottom: 1px solid var(--hairline);
    opacity: 0;
    transform: translateY(12px);
    transition: opacity 0.7s ease-out, transform 0.7s ease-out;
  }
  .mounted section {
    opacity: 1;
    transform: translateY(0);
  }
  .mounted section.identity {
    transition-delay: 1.05s;
  }
  .mounted section.encoding {
    transition-delay: 1.2s;
  }
  .mounted section.manifesto {
    transition-delay: 1.35s;
  }
  .mounted section.shortcuts {
    transition-delay: 1.5s;
  }
  section.frontispiece {
    border-bottom: none;
    opacity: 1;
    transform: none;
  }

  h2.section-title {
    font-family: var(--serif-display);
    font-weight: 400;
    font-size: 22px;
    letter-spacing: 0.02em;
    color: var(--ink);
    margin: 0 0 0.5rem;
    line-height: 1.2;
  }

  .section-sub {
    font-family: var(--serif-body);
    font-style: italic;
    font-size: 14px;
    color: var(--ink-mute);
    line-height: 1.55;
    margin: 0 0 2rem;
    max-width: 56ch;
  }
  .section-sub em {
    color: var(--indigo);
    font-style: italic;
  }

  /* ─── IDENTITY CARD ───────────────────────────────────────── */
  .card {
    position: relative;
    background: var(--paper-deep);
    padding: 1.8rem 2rem;
    border: 1px solid var(--hairline);
  }

  .card-corner {
    position: absolute;
    width: 10px;
    height: 10px;
    background: var(--vermilion);
  }
  .card-corner.top-left {
    top: -1px;
    left: -1px;
  }
  .card-corner.top-right {
    top: -1px;
    right: -1px;
  }
  .card-corner.bottom-left {
    bottom: -1px;
    left: -1px;
  }
  .card-corner.bottom-right {
    bottom: -1px;
    right: -1px;
  }

  .card-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 1rem;
  }
  .card-label {
    font-family: var(--mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.2em;
    color: var(--ink-mute);
  }

  .copy-btn {
    background: transparent;
    border: 1px solid var(--ink-faint);
    color: var(--ink-soft);
    font-family: var(--mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.15em;
    padding: 4px 10px;
    cursor: pointer;
    transition: all 0.18s;
  }
  .copy-btn:hover {
    border-color: var(--ink);
    color: var(--ink);
  }
  .copy-btn.copied {
    background: var(--ink);
    color: var(--paper);
    border-color: var(--ink);
  }

  .fingerprint {
    font-family: var(--mono);
    font-weight: 400;
    font-size: clamp(15px, 2.2vw, 19px);
    letter-spacing: 0.1em;
    line-height: 1.4;
    color: var(--ink);
    padding: 0.5rem 0 1.2rem;
    border-bottom: 1px solid var(--hairline);
    word-break: break-all;
  }

  .card-meta {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1rem;
    margin-top: 1rem;
  }
  .meta-label {
    display: block;
    font-family: var(--mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.2em;
    color: var(--ink-mute);
    margin-bottom: 4px;
  }
  .meta-value {
    font-family: var(--serif-body);
    font-size: 16px;
    color: var(--ink);
  }

  /* ─── ENCODING ────────────────────────────────────────────── */
  .encoder-active {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 1rem 1.2rem;
    background: var(--paper-deep);
    border-left: 3px solid var(--indigo);
    font-family: var(--serif-body);
    font-size: 16px;
    font-style: italic;
    color: var(--ink);
  }
  .encoder-active .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--indigo);
    animation: dot-pulse 2.4s ease-in-out infinite;
    flex-shrink: 0;
  }
  @keyframes dot-pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.4;
    }
  }

  .encoders-detail {
    margin-top: 1rem;
    font-family: var(--serif-body);
  }
  .encoders-detail summary {
    cursor: pointer;
    color: var(--ink-mute);
    font-size: 13px;
    list-style: none;
    display: flex;
    align-items: center;
    gap: 8px;
    user-select: none;
  }
  .encoders-detail summary::-webkit-details-marker {
    display: none;
  }
  .summary-arrow {
    transition: transform 0.2s;
    font-size: 10px;
    color: var(--vermilion);
  }
  .encoders-detail[open] .summary-arrow {
    transform: rotate(180deg);
  }
  .encoders-detail ul {
    list-style: none;
    padding: 1rem 0 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .encoders-detail li {
    font-family: var(--mono);
    font-size: 12px;
    color: var(--ink-soft);
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .tag-hw,
  .tag-mode {
    font-family: var(--mono);
    font-weight: 500;
    font-size: 9px;
    letter-spacing: 0.1em;
    padding: 2px 6px;
    border: 1px solid var(--ink-faint);
    color: var(--ink-mute);
    min-width: 28px;
    text-align: center;
  }
  .tag-hw.on {
    background: var(--ink);
    color: var(--paper);
    border-color: var(--ink);
  }
  .tag-mode.async {
    background: var(--vermilion);
    color: var(--paper);
    border-color: var(--vermilion);
  }
  .enc-name {
    flex: 1;
  }
  .encoders-detail .hint {
    margin: 1rem 0 0;
    font-style: italic;
    font-size: 13px;
    color: var(--ink-mute);
    line-height: 1.55;
  }
  .encoders-detail .hint em {
    color: var(--indigo);
    font-weight: 500;
  }
  .encoders-detail .hint code {
    font-family: var(--mono);
    font-size: 11px;
    color: var(--ink-soft);
    background: var(--paper-deep);
    padding: 1px 5px;
  }

  /* ─── MANIFESTO ───────────────────────────────────────────── */
  .manifesto-body {
    column-count: 1;
  }
  .manifesto-body p {
    font-family: var(--serif-body);
    font-size: 16px;
    line-height: 1.7;
    color: var(--ink-soft);
    margin: 0 0 1rem;
  }
  .manifesto-body p:first-of-type::first-letter {
    font-family: var(--serif-display);
    font-size: 3.2em;
    float: left;
    line-height: 0.9;
    padding: 5px 8px 0 0;
    color: var(--vermilion);
  }
  .manifesto-body em {
    color: var(--indigo);
    font-style: italic;
    font-weight: 500;
  }
  .manifesto-body strong {
    color: var(--ink);
    font-weight: 600;
    font-style: italic;
    font-family: var(--serif-display);
  }

  /* ─── SHORTCUTS — kbd as ink stamps ───────────────────────── */
  .shortcut-list {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 1rem 1.5rem;
    margin: 0;
  }
  .combo {
    display: flex;
    align-items: center;
    gap: 4px;
    font-family: var(--mono);
    color: var(--ink-mute);
  }
  .combo > span {
    font-weight: 300;
    margin: 0 1px;
  }
  .shortcut-list kbd {
    display: inline-block;
    font-family: var(--mono);
    font-weight: 500;
    font-size: 11px;
    color: var(--paper);
    background: var(--ink);
    padding: 4px 8px;
    letter-spacing: 0.05em;
    min-width: 14px;
    text-align: center;
    border: none;
    border-radius: 0;
  }
  .shortcut-list dd {
    margin: 0;
    font-family: var(--serif-body);
    font-style: italic;
    font-size: 15px;
    color: var(--ink-soft);
    align-self: center;
  }
  .combo.edge .edge-glyph {
    background: var(--vermilion);
    color: var(--paper);
    padding: 4px 8px;
    font-size: 12px;
    font-weight: 700;
  }

  /* ─── COLOPHON — colophon de fin (style colophon de livre) ── */
  .colophon {
    margin-top: 3rem;
    padding-top: 2.5rem;
    border-top: 1px solid var(--hairline);
    display: grid;
    grid-template-columns: 1fr 1fr 1fr;
    gap: 1.5rem;
    opacity: 0;
    transition: opacity 0.8s ease-out;
    transition-delay: 1.7s;
  }
  .mounted .colophon {
    opacity: 1;
  }
  @media (max-width: 600px) {
    .colophon {
      grid-template-columns: 1fr;
    }
  }
  .col-label {
    display: block;
    font-family: var(--mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.2em;
    color: var(--ink-mute);
    margin-bottom: 6px;
  }
  .col-value {
    font-family: var(--serif-body);
    font-size: 14px;
    color: var(--ink-soft);
    line-height: 1.4;
  }
  .col-value.mono {
    font-family: var(--mono);
    font-size: 12px;
  }
  .col-value.path {
    word-break: break-all;
  }

  /* ─── SEAL — sceau circulaire en bas comme un hanko ──────── */
  .seal {
    margin: 3rem auto 0;
    width: 90px;
    height: 90px;
    border-radius: 50%;
    border: 1.5px solid var(--vermilion);
    position: relative;
    display: grid;
    place-items: center;
    opacity: 0;
    transform: scale(0.9);
    transition: opacity 0.9s, transform 0.9s;
    transition-delay: 1.9s;
  }
  .mounted .seal {
    opacity: 0.7;
    transform: scale(1);
  }
  .seal::before {
    content: "";
    position: absolute;
    inset: 5px;
    border: 1px solid var(--vermilion);
    border-radius: 50%;
    opacity: 0.4;
  }
  .seal-inner {
    font-family: var(--serif-display);
    font-size: 11px;
    color: var(--vermilion);
    letter-spacing: 0.12em;
    line-height: 1.3;
    text-align: center;
    max-width: 60px;
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
  }
  .seal-inner span {
    display: inline-block;
    padding: 0 1px;
  }

  /* ─── REDUCED MOTION ──────────────────────────────────────── */
  @media (prefers-reduced-motion: reduce) {
    .washi *,
    .washi *::before,
    .washi *::after {
      transition-duration: 0.01ms !important;
      animation-duration: 0.01ms !important;
    }
    .washi section {
      opacity: 1;
      transform: none;
    }
    .mounted .kana {
      opacity: 0.2;
    }
  }
</style>
