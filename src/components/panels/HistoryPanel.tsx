// Undo tree (CORE-09…13): every entry, branches included; click to jump.
import { useEffect, useState } from "react";
import { api, type HistoryItem } from "../../ipc/api";
import * as A from "../../state/actions";
import type { Tab } from "../../state/tabs";

function time(ms: number): string {
  return new Date(ms).toLocaleTimeString("ru-RU", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function depthOf(items: HistoryItem[]): Map<number, number> {
  const byId = new Map(items.map((i) => [i.id, i]));
  const depth = new Map<number, number>();
  for (const i of items) {
    // Branch depth: how many times the path to the root leaves the redo path.
    let d = 0;
    let n: HistoryItem | undefined = i;
    while (n && n.parent !== null) {
      const parent = byId.get(n.parent);
      const siblings = items.filter((x) => x.parent === n!.parent);
      if (siblings.length > 1 && siblings[siblings.length - 1].id !== n.id) d++;
      n = parent;
    }
    depth.set(i.id, d);
  }
  return depth;
}

export function HistoryPanel({ tab }: { tab: Tab }) {
  const [items, setItems] = useState<HistoryItem[]>([]);
  useEffect(() => {
    let alive = true;
    const load = () =>
      api
        .history(tab.id)
        .then((h) => alive && setItems(h))
        .catch(() => {});
    load();
    const off = A.onHistoryChange(load);
    return () => {
      alive = false;
      off();
    };
  }, [tab.id, tab.info.version]);

  const depth = depthOf(items);
  const ordered = [...items].reverse();
  return (
    <>
      <div className="ph">
        <span className="cap">История правок</span>
        <div className="grow" />
        <span className="tag">{items.length - 1}</span>
      </div>
      {items.length <= 1 ? (
        <div className="empty">
          <span>
            Правок пока нет. История — это дерево: после отмены и новой правки старая ветка не
            теряется, к ней можно вернуться отсюда.
          </span>
        </div>
      ) : (
        <div className="list" role="tree" aria-label="История правок">
          {ordered.map((h, i) => (
            <button
              type="button"
              key={h.id}
              role="treeitem"
              aria-current={h.current}
              className={`li${h.current ? " cur" : ""}${h.onRedoPath || h.current ? "" : " off"}`}
              style={{
                paddingLeft: 8 + (depth.get(h.id) ?? 0) * 14,
                ["--delay" as string]: `${Math.min(i, 12) * 0.02}s`,
              }}
              onClick={() => A.historyGoto(h.id)}
              title={h.saved ? "Сохранённое состояние" : undefined}
            >
              <span className={`node${h.current ? " cur" : ""}${h.saved ? " saved" : ""}`} />
              <span className="t">
                <b>{h.id === 0 ? "Исходный файл" : label(h.label)}</b>
                <span>
                  {time(h.timeMs)}
                  {h.saved ? " · сохранено" : ""}
                </span>
              </span>
              <span className="v">#{h.id}</span>
            </button>
          ))}
        </div>
      )}
    </>
  );
}

const LABELS: Record<string, string> = {
  Typing: "Ввод",
  Overwrite: "Замена",
  Insert: "Вставка",
  Delete: "Удаление",
  Replace: "Замена диапазона",
  Paste: "Вставка из буфера",
  Fill: "Заполнение",
  "Insert bytes": "Вставка байт",
  "Insert file": "Вставка файла",
  Extend: "Расширение",
  "Byte swap": "Смена порядка байт",
  Reverse: "Разворот",
  "Move block": "Перемещение блока",
  New: "Новый файл",
  Open: "Открытие",
  "Recovered session": "Восстановлено после сбоя",
};

function label(l: string) {
  return LABELS[l] ?? l;
}
