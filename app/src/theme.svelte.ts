// Helper module-level qui applique le theme choisi a <html>.
//
// Trois valeurs possibles cote backend (cf. okvm-config::Theme) :
//
// - "System" : on retire data-theme → CSS auto via prefers-color-scheme
// - "Light"  : data-theme="light"
// - "Dark"   : data-theme="dark"

export type ThemeMode = "System" | "Light" | "Dark";

let _current = $state<ThemeMode>("System");

export function getTheme(): ThemeMode {
  return _current;
}

export function setTheme(mode: ThemeMode) {
  _current = mode;
  applyTheme(mode);
}

function applyTheme(mode: ThemeMode) {
  const html = document.documentElement;
  if (mode === "System") {
    html.removeAttribute("data-theme");
  } else if (mode === "Light") {
    html.setAttribute("data-theme", "light");
  } else {
    html.setAttribute("data-theme", "dark");
  }
}
