// Editor area: split panes of one file (DISP-20/22) and another file side
// by side with synchronised scrolling (DISP-21, DIFF-06).
import { useEffect } from "react";
import { HexGrid, linkedScroll } from "../grid/HexGrid";
import { useDiff } from "../state/diff";
import { type Tab, useTabs } from "../state/tabs";
import { useUi } from "../state/ui";
import { DiffBar } from "./DiffBar";
import { Icon } from "./icons";

function PaneHeader({ tab, pane }: { tab: Tab; pane: number }) {
  const p = tab.panes[pane - 1];
  const set = (patch: Partial<typeof p>) =>
    useTabs.getState().update(tab.id, (t) => ({
      panes: t.panes.map((x, i) => (i === pane - 1 ? { ...x, ...patch } : x)),
    }));
  return (
    <div className="pane-h">
      <span className="cap">Панель {pane + 1}</span>
      <select
        className="sel-input"
        value={String(p?.bytesPerRow ?? "")}
        aria-label="Байт в строке"
        onChange={(e) => {
          const v = e.target.value;
          set({ bytesPerRow: v === "" ? undefined : v === "auto" || v === "pow2" ? v : Number(v) });
        }}
      >
        <option value="">байт/строка: как в настройках</option>
        {[8, 16, 32, 64].map((n) => (
          <option key={n} value={n}>
            {n} байт
          </option>
        ))}
        <option value="auto">по ширине</option>
      </select>
      <select
        className="sel-input"
        value={p?.cellView ?? ""}
        aria-label="Вид ячеек"
        onChange={(e) => set({ cellView: e.target.value || undefined })}
      >
        <option value="">вид: как в настройках</option>
        {["hex", "dec", "oct", "bin", "signed", "float", "char", "hexii"].map((v) => (
          <option key={v}>{v}</option>
        ))}
      </select>
      <div className="grow" />
      <button
        type="button"
        className="ib"
        aria-label="Закрыть панель"
        onClick={() =>
          useTabs
            .getState()
            .update(tab.id, (t) => ({ panes: t.panes.filter((_, i) => i !== pane - 1) }))
        }
      >
        <Icon.close />
      </button>
    </div>
  );
}

export function EditorArea({ tab }: { tab: Tab }) {
  const compare = useUi((u) => u.compare);
  const other = useTabs((st) =>
    compare && (compare.a === tab.id || compare.b === tab.id)
      ? st.tabs.find((t) => t.id === (compare.a === tab.id ? compare.b : compare.a))
      : undefined,
  );

  // Link scrolling of the two files (DISP-21).
  useEffect(() => {
    if (!other || !compare?.sync) return;
    linkedScroll.pairs.set(tab.id, [other.id]);
    linkedScroll.pairs.set(other.id, [tab.id]);
    return () => {
      linkedScroll.pairs.delete(tab.id);
      linkedScroll.pairs.delete(other.id);
    };
  }, [tab.id, other?.id, compare?.sync]);

  const column = (t: Tab, active: boolean) => (
    <div
      className="editor-col"
      key={t.id}
      onMouseDown={() => !active && useTabs.getState().activate(t.id)}
    >
      {other && (
        <div className={`pane-h${active ? " on" : ""}`}>
          <span className="badge">{active ? "активная" : "рядом"}</span>
          <span className="tn">{t.info.name}</span>
          <div className="grow" />
          {active && (
            <>
              <button
                type="button"
                className={`tog${compare?.sync ? " on" : ""}`}
                onClick={() =>
                  compare && useUi.getState().set({ compare: { ...compare, sync: !compare.sync } })
                }
              >
                синхронно
              </button>
              <button
                type="button"
                className="ib"
                aria-label="Закрыть сравнение"
                onClick={() => useUi.getState().set({ compare: null })}
              >
                <Icon.close />
              </button>
            </>
          )}
        </div>
      )}
      <HexGrid key={`${t.id}-0`} tab={t} pane={0} />
      {active &&
        t.panes.map((_, i) => (
          // biome-ignore lint/suspicious/noArrayIndexKey: panes are positional
          <div key={`${t.id}-${i + 1}`} className="pane">
            <PaneHeader tab={t} pane={i + 1} />
            <HexGrid tab={t} pane={i + 1} />
          </div>
        ))}
    </div>
  );

  const diffOn = useDiff((d) => d.a !== null && (d.a === tab.id || d.b === tab.id));
  return (
    <div className="editor-wrap">
      {diffOn && other && <DiffBar />}
      <div className="editor-area">
        {other && compare?.b === tab.id ? (
          <>
            {column(other, false)}
            {column(tab, true)}
          </>
        ) : (
          <>
            {column(tab, true)}
            {other && column(other, false)}
          </>
        )}
        {tab.panes.length > 0 && (
          <button
            type="button"
            className={`pane-sync tog${tab.paneSync ? " on" : ""}`}
            title="Синхронная прокрутка панелей"
            onClick={() => useTabs.getState().update(tab.id, (t) => ({ paneSync: !t.paneSync }))}
          >
            ⇅ синхронно
          </button>
        )}
      </div>
    </div>
  );
}
