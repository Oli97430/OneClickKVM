<script lang="ts" module>
  import type { NotificationLevel } from "../ipc";

  export interface ToastInput {
    level: NotificationLevel;
    title: string;
    body: string;
  }

  interface Toast extends ToastInput {
    id: number;
  }

  let nextId = 1;
  // Le store est un signal Svelte 5 module-level, donc partage entre composants.
  let _toasts = $state<Toast[]>([]);

  export function pushNotification(t: ToastInput) {
    const id = nextId++;
    _toasts = [..._toasts, { ...t, id }];
    setTimeout(() => {
      _toasts = _toasts.filter((x) => x.id !== id);
    }, 5_000);
  }

  export function dismissNotification(id: number) {
    _toasts = _toasts.filter((x) => x.id !== id);
  }

  export function getToasts() {
    return _toasts;
  }
</script>

<script lang="ts">
  // Acces reactif au store module-level via une wrapper-derived
  const toasts = $derived(getToasts());
</script>

<div class="toast-stack" aria-live="polite">
  {#each toasts as t (t.id)}
    <div class="toast {t.level}">
      <div class="toast-body">
        <strong>{t.title}</strong>
        <div class="body">{t.body}</div>
      </div>
      <button class="close" onclick={() => dismissNotification(t.id)}>×</button>
    </div>
  {/each}
</div>

<style>
  .toast-stack {
    position: fixed;
    bottom: 1.25rem;
    right: 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    z-index: 1000;
    max-width: 360px;
  }

  .toast {
    background: var(--surface);
    border: 1px solid var(--border);
    border-left: 3px solid var(--fg-muted);
    border-radius: 8px;
    padding: 0.7rem 0.85rem;
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.35);
    animation: slide-in 180ms ease-out;
  }

  .toast.info {
    border-left-color: #60a5fa;
  }
  .toast.success {
    border-left-color: var(--success);
  }
  .toast.warn {
    border-left-color: var(--warn);
  }
  .toast.error {
    border-left-color: var(--error);
  }

  .toast-body {
    flex: 1;
    min-width: 0;
  }

  .toast strong {
    display: block;
    font-size: 0.88rem;
    margin-bottom: 0.15rem;
  }

  .toast .body {
    font-size: 0.8rem;
    color: var(--fg-muted);
    word-wrap: break-word;
  }

  .close {
    background: transparent;
    border: none;
    color: var(--fg-muted);
    font-size: 1.25rem;
    line-height: 1;
    cursor: pointer;
    padding: 0 0.25rem;
  }

  .close:hover {
    color: var(--fg);
  }

  @keyframes slide-in {
    from {
      transform: translateX(20px);
      opacity: 0;
    }
    to {
      transform: translateX(0);
      opacity: 1;
    }
  }
</style>
