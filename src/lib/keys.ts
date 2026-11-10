// Keyboard chords: normalisation and display (UI-03).

export const isMac =
  typeof navigator !== "undefined" &&
  /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

const KEY_NAMES: Record<string, string> = {
  ArrowLeft: "Left",
  ArrowRight: "Right",
  ArrowUp: "Up",
  ArrowDown: "Down",
  " ": "Space",
  Escape: "Esc",
  "+": "Plus",
  "=": "Plus",
  "-": "Minus",
  _: "Minus",
};

/** `Mod` is Cmd on macOS and Ctrl elsewhere; `Ctrl` is the real Control on macOS. */
export function chordOf(e: KeyboardEvent): string {
  let key = KEY_NAMES[e.key] ?? e.key;
  if (key.length === 1) key = key.toUpperCase();
  // Use the physical key for letters when Alt changes the character (macOS).
  if (e.altKey && /^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
  if (e.altKey && /^Digit[0-9]$/.test(e.code)) key = e.code.slice(5);
  if (e.code === "BracketLeft" && (e.metaKey || e.ctrlKey)) key = "[";
  if (e.code === "BracketRight" && (e.metaKey || e.ctrlKey)) key = "]";
  const parts: string[] = [];
  if (isMac) {
    if (e.ctrlKey) parts.push("Ctrl");
    if (e.metaKey) parts.push("Mod");
  } else {
    if (e.ctrlKey) parts.push("Mod");
    if (e.metaKey) parts.push("Meta");
  }
  if (e.altKey) parts.push("Alt");
  // Shift is part of the character for printable symbols like "<" or "?".
  if (e.shiftKey && (key.length > 1 || /[A-Z0-9]/.test(key))) parts.push("Shift");
  if (["Control", "Meta", "Alt", "Shift"].includes(key)) return "";
  parts.push(key);
  return parts.join("+");
}

const MAC_SYMBOLS: Record<string, string> = {
  Mod: "⌘",
  Ctrl: "⌃",
  Alt: "⌥",
  Shift: "⇧",
  Left: "←",
  Right: "→",
  Up: "↑",
  Down: "↓",
  Enter: "↵",
  Esc: "esc",
  Backspace: "⌫",
  Delete: "⌦",
  Plus: "+",
  Minus: "−",
  PageUp: "PgUp",
  PageDown: "PgDn",
};

/** Human-readable form of a binding like `Shift+Mod+Z`, chords separated by space. */
export function displayKeys(binding: string | undefined): string {
  if (!binding) return "";
  return binding
    .split(" ")
    .map((chord) => {
      const parts = chord.split("+").filter(Boolean);
      if (isMac) return parts.map((p) => MAC_SYMBOLS[p] ?? p).join("");
      return parts
        .map((p) => (p === "Mod" ? "Ctrl" : p === "Plus" ? "+" : p === "Minus" ? "−" : p))
        .join("+");
    })
    .join(" ");
}
