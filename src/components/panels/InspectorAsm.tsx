// The instruction under the cursor for the chosen architecture (INSP-14).
import { useEffect, useState } from "react";
import { type ArchInfo, re } from "../../ipc/re";
import { useSettings } from "../../state/settings";
import type { Tab } from "../../state/tabs";

let archCache: ArchInfo[] | null = null;

export function InspectorAsm({ tab }: { tab: Tab }) {
  const arch = useSettings((s) => s.s.inspectorArch);
  const set = useSettings((s) => s.set);
  const [arches, setArches] = useState<ArchInfo[]>(archCache ?? []);
  const [text, setText] = useState<string | null>(null);
  useEffect(() => {
    if (!archCache)
      re.arches()
        .then((a) => {
          archCache = a;
          setArches(a);
        })
        .catch(() => {});
  }, []);
  useEffect(() => {
    if (!arch) return setText(null);
    let alive = true;
    re.disasm(tab.id, tab.cursor, 16, tab.cursor, arch, 1)
      .then((l) => alive && setText(l[0] ? `${l[0].mnemonic} ${l[0].operands}`.trim() : null))
      .catch(() => alive && setText(null));
    return () => {
      alive = false;
    };
  }, [tab.id, tab.cursor, tab.info.version, arch]);
  return (
    <div className="ir" style={{ gridTemplateColumns: "92px minmax(0,1fr)" }}>
      <select
        className="sel-input ik"
        aria-label="Архитектура инструкции"
        value={arch}
        onChange={(e) => set({ inspectorArch: e.target.value })}
      >
        <option value="">Инструкция…</option>
        {arches.map((a) => (
          <option key={a.arch} value={a.arch}>
            {a.label}
          </option>
        ))}
      </select>
      <span className="iv mono">{arch ? (text ?? "—") : ""}</span>
    </div>
  );
}
