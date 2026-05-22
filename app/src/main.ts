// Diagnostic wrapper : si quoi que ce soit plante au boot, on l'affiche
// dans le DOM au lieu de laisser une page blanche silencieuse.

import { mount } from "svelte";
import "./styles/global.css";
import App from "./App.svelte";

function showError(msg: string) {
  const root = document.getElementById("app") ?? document.body;
  root.innerHTML = `
    <div style="padding:2rem;font-family:Segoe UI,sans-serif;color:#fca5a5;background:#0f1115;min-height:100vh;line-height:1.5;">
      <h1 style="margin:0 0 1rem 0;color:#fff;">⚠ OneClick KVM — erreur au demarrage</h1>
      <pre style="background:#181b22;padding:1rem;border-radius:8px;overflow:auto;white-space:pre-wrap;word-break:break-word;border:1px solid #ef4444;">${msg.replace(/[<>&]/g, (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;" }[c]!))}</pre>
      <p style="color:#9aa0aa;font-size:0.85rem;margin-top:1rem;">F12 pour console.</p>
    </div>
  `;
}

window.addEventListener("error", (ev) => {
  showError(`${ev.message}\n\n${ev.filename}:${ev.lineno}:${ev.colno}\n\n${ev.error?.stack ?? "(pas de stack)"}`);
});
window.addEventListener("unhandledrejection", (ev) => {
  showError(`Unhandled promise rejection:\n\n${ev.reason?.stack ?? String(ev.reason)}`);
});

try {
  const target = document.getElementById("app");
  if (!target) {
    throw new Error("Element #app introuvable dans index.html");
  }
  mount(App, { target });
} catch (e) {
  const err = e as Error;
  showError(`Boot crash:\n${err.message}\n\n${err.stack ?? "(pas de stack)"}`);
}
