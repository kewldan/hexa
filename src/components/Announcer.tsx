// Screen reader announcements of the cursor cell (UI-17) and the opt-in
// update check at startup (UI-15).
import { useEffect, useState } from "react";
import { api } from "../ipc/api";
import { hexN } from "../lib/format";
import { useSettings } from "../state/settings";
import { hasSelection, selection, type Tab } from "../state/tabs";

let updateChecked = false;

export function Announcer({ tab }: { tab: Tab }) {
  const sr = useSettings((s) => s.s.screenReader);
  const updates = useSettings((s) => s.s.checkUpdates);
  const [text, setText] = useState("");
  useEffect(() => {
    if (!updates || updateChecked) return;
    updateChecked = true;
    void import("./dialogs/ToolDialogs").then((m) => m.checkUpdates(false));
  }, [updates]);
  useEffect(() => {
    if (!sr) return;
    const t = setTimeout(async () => {
      if (hasSelection(tab)) {
        const r = selection(tab);
        setText(`Выделено ${r.end - r.start} байт, с 0x${hexN(r.start)} по 0x${hexN(r.end - 1)}`);
        return;
      }
      let byte = "";
      if (tab.cursor < tab.info.len) {
        const b = (await api.read(tab.id, tab.cursor, 1).catch(() => new Uint8Array()))[0];
        if (b !== undefined) {
          const ch = b >= 0x20 && b < 0x7f ? `, символ ${String.fromCharCode(b)}` : "";
          byte = `, байт 0x${hexN(b, 2)} (${b})${ch}`;
        }
      } else byte = ", конец файла";
      setText(`Смещение 0x${hexN(tab.cursor)}${byte}`);
    }, 250);
    return () => clearTimeout(t);
  }, [sr, tab.id, tab.cursor, tab.anchor, tab.info.version]);
  if (!sr) return null;
  return (
    <div className="sr-only" aria-live="polite" aria-atomic="true">
      {text}
    </div>
  );
}
