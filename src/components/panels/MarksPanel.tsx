// Bookmarks, labels and annotations (NAV-14…18).
import { open as openDialog_, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { useMemo, useState } from "react";
import { api, type Mark, type MarkKind } from "../../ipc/api";
import { fmtSize, hexN } from "../../lib/format";
import * as A from "../../state/actions";
import type { Tab } from "../../state/tabs";
import { openDialog, toast } from "../../state/ui";
import { Icon } from "../icons";

export const MARK_COLORS: Record<string, string> = {
  blue: "#2F6BF0",
  orange: "#E8890C",
  green: "#1F9D74",
  pink: "#D6336C",
  purple: "#7C4DFF",
  red: "#DC2626",
  yellow: "#CA8A04",
  gray: "#6B6B73",
};

export function markColor(c: string): string {
  return MARK_COLORS[c] ?? c;
}

const KIND_LABEL: Record<MarkKind, string> = {
  bookmark: "Закладка",
  label: "Метка",
  annotation: "Заметка",
};

type Sort = "offset" | "name" | "kind";

export function MarksPanel({ tab }: { tab: Tab }) {
  const [q, setQ] = useState("");
  const [sort, setSort] = useState<Sort>("offset");
  const list = useMemo(() => {
    const f = q.toLowerCase();
    const items = tab.marks.filter(
      (m) =>
        !f ||
        m.name.toLowerCase().includes(f) ||
        m.note.toLowerCase().includes(f) ||
        hexN(m.start).toLowerCase().includes(f),
    );
    items.sort((a, b) =>
      sort === "name"
        ? a.name.localeCompare(b.name)
        : sort === "kind"
          ? a.kind.localeCompare(b.kind) || a.start - b.start
          : a.start - b.start,
    );
    return items;
  }, [tab.marks, q, sort]);

  const exportMarks = async () => {
    const path = await saveDialog({ defaultPath: `${tab.info.name}.marks.json` });
    if (!path) return;
    try {
      await api.writeTextFile(path, JSON.stringify(tab.marks, null, 2));
      toast(`Экспортировано: ${tab.marks.length}`);
    } catch (e) {
      A.report(e);
    }
  };
  const importMarks = async () => {
    const path = await openDialog_({
      multiple: false,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!path || Array.isArray(path)) return;
    try {
      const data = JSON.parse(await api.readTextFile(path)) as Partial<Mark>[];
      await api.marksSet(tab.id, [...tab.marks, ...data]);
      await A.refreshMarks(tab.id);
      toast(`Импортировано: ${data.length}`);
    } catch (e) {
      A.report(e);
    }
  };

  return (
    <>
      <div className="ph">
        <span className="cap">Закладки</span>
        <span className="tag">{tab.marks.length}</span>
        <div className="grow" />
        <button
          type="button"
          className="chip"
          onClick={() => A.addMark("bookmark")}
          title="Закладка на выделение (⌘D)"
        >
          <Icon.plus /> Закладка
        </button>
      </div>
      <div style={{ display: "flex", gap: 6, padding: "0 4px" }}>
        <button type="button" className="chip" onClick={() => A.addMark("label")}>
          Метка
        </button>
        <button type="button" className="chip" onClick={() => A.addMark("annotation")}>
          Заметка
        </button>
        <div className="grow" />
        <button type="button" className="chip" onClick={importMarks} title="Импорт из JSON">
          Импорт
        </button>
        <button
          type="button"
          className="chip"
          onClick={exportMarks}
          title="Экспорт в JSON"
          disabled={!tab.marks.length}
        >
          Экспорт
        </button>
      </div>
      <div style={{ display: "flex", gap: 6, padding: "0 4px" }}>
        <input
          className="inp"
          placeholder="Поиск по имени, заметке, смещению"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => e.stopPropagation()}
        />
        <select
          className="sel-input"
          value={sort}
          onChange={(e) => setSort(e.target.value as Sort)}
          aria-label="Сортировка"
        >
          <option value="offset">по смещению</option>
          <option value="name">по имени</option>
          <option value="kind">по типу</option>
        </select>
      </div>
      {list.length === 0 ? (
        <div className="empty">
          <span>
            {tab.marks.length
              ? "Ничего не найдено."
              : "Закладок пока нет. Выделите байты и нажмите ⌘D — закладка поедет вместе с данными при вставках и удалениях. Метки можно использовать в «Перейти»: header+4."}
          </span>
        </div>
      ) : (
        <div className="list">
          {list.map((m, i) => (
            <div
              key={m.id}
              className={`li${m.orphaned ? " off" : ""}`}
              style={{
                ["--delay" as string]: `${Math.min(i, 10) * 0.02}s`,
                gridTemplateColumns: "12px minmax(0,1fr) auto",
              }}
            >
              <span className="dotc" style={{ background: markColor(m.color) }} />
              <button
                type="button"
                className="t"
                style={{ textAlign: "left" }}
                onClick={() => A.selectRange(m.start, Math.max(m.end, m.start + 1))}
                onDoubleClick={() => openDialog({ kind: "mark", mark: m })}
                title={m.note || "Двойной клик — изменить"}
              >
                <b>{m.name || KIND_LABEL[m.kind]}</b>
                <span>
                  0x{hexN(m.start, 8)} · {fmtSize(m.end - m.start)} · {KIND_LABEL[m.kind]}
                  {m.orphaned ? " · данные удалены" : ""}
                </span>
              </button>
              <span style={{ display: "flex", gap: 2 }}>
                <button
                  type="button"
                  className="ib"
                  aria-label="Изменить"
                  onClick={() => openDialog({ kind: "mark", mark: m })}
                >
                  <Icon.gear />
                </button>
                <button
                  type="button"
                  className="ib"
                  aria-label="Удалить"
                  onClick={() => api.marksRemove(tab.id, m.id).then(() => A.refreshMarks(tab.id))}
                >
                  <Icon.close />
                </button>
              </span>
            </div>
          ))}
        </div>
      )}
    </>
  );
}
