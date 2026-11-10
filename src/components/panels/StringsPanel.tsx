// Strings in the selection or the whole file (ANL-11).
import { save as saveFile } from "@tauri-apps/plugin-dialog";
import { useMemo, useState } from "react";
import { anl, type FoundString, type StringEncoding } from "../../ipc/analysis";
import { api } from "../../ipc/api";
import { fmtSize, hexN } from "../../lib/format";
import * as A from "../../state/actions";
import "../../styles/analysis.css";
import { hasSelection, selection, type Tab } from "../../state/tabs";
import { toast } from "../../state/ui";

const ENC: { id: StringEncoding; label: string }[] = [
  { id: "ascii", label: "ASCII" },
  { id: "utf8", label: "UTF-8" },
  { id: "utf16Le", label: "UTF-16LE" },
  { id: "utf16Be", label: "UTF-16BE" },
];
const ENC_SHORT: Record<StringEncoding, string> = {
  ascii: "A",
  utf8: "U8",
  utf16Le: "U16",
  utf16Be: "U16B",
};
const PAGE = 1000;

export function StringsPanel({ tab }: { tab: Tab }) {
  const [minLen, setMinLen] = useState(4);
  const [encs, setEncs] = useState<StringEncoding[]>(["ascii", "utf8", "utf16Le"]);
  const [filter, setFilter] = useState("");
  const [list, setList] = useState<FoundString[] | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [busy, setBusy] = useState(false);
  const [limit, setLimit] = useState(PAGE);
  const range = hasSelection(tab) ? selection(tab) : { start: 0, end: tab.info.len };
  const run = () => {
    setBusy(true);
    setLimit(PAGE);
    anl
      .strings(tab.id, range.start, range.end, { encodings: encs, minLen, maxResults: 200_000 })
      .then((r) => {
        setList(r.strings);
        setTruncated(r.truncated);
      })
      .catch(A.report)
      .finally(() => setBusy(false));
  };
  const shown = useMemo(() => {
    if (!list) return [];
    const f = filter.toLowerCase();
    return f ? list.filter((s) => s.text.toLowerCase().includes(f)) : list;
  }, [list, filter]);
  const exportList = async () => {
    const path = await saveFile({
      defaultPath: `${tab.info.name}.strings.csv`,
      filters: [
        { name: "CSV", extensions: ["csv"] },
        { name: "Текст", extensions: ["txt"] },
      ],
    });
    if (!path) return;
    const csv = path.toLowerCase().endsWith(".csv");
    const esc = (s: string) => `"${s.replaceAll('"', '""')}"`;
    const text = csv
      ? `offset,length,encoding,text\n${shown.map((s) => `${s.offset},${s.len},${s.encoding},${esc(s.text)}`).join("\n")}\n`
      : `${shown.map((s) => `${hexN(s.offset, 8)}  ${s.text}`).join("\n")}\n`;
    await api
      .writeTextFile(path, text)
      .then(() => toast(`Сохранено: ${shown.length}`))
      .catch(A.report);
  };
  return (
    <>
      <div className="ph">
        <span className="cap">Строки</span>
        {list && <span className="tag">{list.length.toLocaleString("ru")}</span>}
        <div className="grow" />
        <button type="button" className="chip" onClick={run} disabled={busy}>
          {busy ? "Ищу…" : list ? "Обновить" : "Найти"}
        </button>
      </div>
      <div
        style={{
          display: "flex",
          gap: 6,
          padding: "0 4px",
          flexWrap: "wrap",
          alignItems: "center",
        }}
      >
        {ENC.map((e) => (
          <label key={e.id} className="chk" style={{ margin: 0 }}>
            <input
              type="checkbox"
              checked={encs.includes(e.id)}
              onChange={(ev) =>
                setEncs(ev.target.checked ? [...encs, e.id] : encs.filter((x) => x !== e.id))
              }
            />
            {e.label}
          </label>
        ))}
        <label className="chk" style={{ margin: 0 }}>
          от
          <input
            className="inp"
            style={{ width: 44 }}
            type="number"
            min={1}
            value={minLen}
            onChange={(e) => setMinLen(Math.max(1, Number(e.target.value) || 1))}
            onKeyDown={(e) => e.stopPropagation()}
          />
          симв.
        </label>
      </div>
      {list && (
        <div style={{ display: "flex", gap: 6, padding: "6px 4px 0" }}>
          <input
            className="inp"
            placeholder="Фильтр"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            onKeyDown={(e) => e.stopPropagation()}
          />
          <button type="button" className="chip" onClick={exportList} disabled={!shown.length}>
            Экспорт
          </button>
        </div>
      )}
      {!list ? (
        <div className="empty">
          <span>
            Поиск строк в {hasSelection(tab) ? "выделении" : "файле"} (
            {fmtSize(range.end - range.start)}) в нескольких кодировках сразу.
          </span>
        </div>
      ) : (
        <div className="list" style={{ gap: 0 }}>
          {shown.slice(0, limit).map((s) => (
            <button
              type="button"
              key={`${s.offset}-${s.encoding}`}
              className="str-row"
              title={s.text}
              onClick={() => A.selectRange(s.offset, s.offset + s.len)}
            >
              <span className="mono">{hexN(s.offset, 8)}</span>
              <span className="enc">{ENC_SHORT[s.encoding]}</span>
              <span className="tx">{s.text}</span>
            </button>
          ))}
          {shown.length > limit && (
            <button type="button" className="chip" onClick={() => setLimit(limit + PAGE)}>
              Ещё {Math.min(PAGE, shown.length - limit)}
            </button>
          )}
          {truncated && (
            <div className="note">Показаны первые {list.length.toLocaleString("ru")} строк.</div>
          )}
        </div>
      )}
    </>
  );
}
