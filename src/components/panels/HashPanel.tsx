// Checksums, CRCs and crypto hashes of the selection or the file (ANL-01, ANL-02).
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useEffect, useMemo, useState } from "react";
import { type AlgoInfo, api, type CrcParams, type HashResult } from "../../ipc/api";
import { fmtSize } from "../../lib/format";
import { report } from "../../state/actions";
import { useSettings } from "../../state/settings";
import { hasSelection, selection, type Tab } from "../../state/tabs";
import { toast } from "../../state/ui";
import { Icon } from "../icons";

const GROUPS: Record<AlgoInfo["group"], string> = {
  checksum: "Контрольные суммы",
  crc: "CRC",
  crypto: "Криптохэши",
  fast: "Быстрые некриптографические",
  fuzzy: "Нечёткие (похожесть)",
};

export function HashPanel({ tab }: { tab: Tab }) {
  const chosen = useSettings((s) => s.s.hashAlgos);
  const set = useSettings((s) => s.set);
  const [algos, setAlgos] = useState<AlgoInfo[]>([]);
  const [results, setResults] = useState<HashResult[]>([]);
  const [job, setJob] = useState<number | null>(null);
  const [progress, setProgress] = useState(0);
  const [picking, setPicking] = useState(false);
  const [filter, setFilter] = useState("");
  const [custom, setCustom] = useState<CrcParams>({
    width: 32,
    poly: 0x04c11db7,
    init: 0xffffffff,
    refin: true,
    refout: true,
    xorout: 0xffffffff,
  });
  const [useCustom, setUseCustom] = useState(false);
  const [expected, setExpected] = useState("");

  useEffect(() => {
    api
      .hashAlgorithms()
      .then(setAlgos)
      .catch(() => {});
  }, []);

  const has = hasSelection(tab);
  const range = has ? selection(tab) : { start: 0, end: tab.info.len };

  const run = async () => {
    if (job) {
      api.cancelJob(job);
      return;
    }
    const ids = useCustom ? [...chosen, "crc:custom"] : chosen;
    if (ids.length === 0) return toast("Выберите алгоритмы", true);
    setResults([]);
    setProgress(0);
    try {
      const id = await api.hashRun(
        tab.id,
        range.start,
        range.end,
        ids,
        useCustom ? custom : null,
        (e) => {
          if (e.type === "progress") setProgress(e.done / Math.max(1, e.total));
          else if (e.type === "done") {
            setResults(e.results);
            setJob(null);
          } else {
            setJob(null);
            report(e.error);
          }
        },
      );
      setJob(id);
    } catch (e) {
      report(e);
    }
  };

  const nameOf = (id: string) =>
    id === "crc:custom" ? "CRC (свои параметры)" : (algos.find((a) => a.id === id)?.name ?? id);
  const shown = useMemo(
    () => algos.filter((a) => a.name.toLowerCase().includes(filter.toLowerCase())),
    [algos, filter],
  );
  const exp = expected.trim().toLowerCase().replace(/^0x/, "");

  return (
    <>
      <div className="ph">
        <span className="cap">Хэши</span>
        <span className="tag">
          {has
            ? `выделение · ${fmtSize(range.end - range.start)}`
            : `файл · ${fmtSize(tab.info.len)}`}
        </span>
        <div className="grow" />
        <button
          type="button"
          className={`chip${picking ? " on" : ""}`}
          onClick={() => setPicking(!picking)}
        >
          Алгоритмы <span className="cnt">{chosen.length}</span>
        </button>
      </div>
      {picking && (
        <div className="list" style={{ maxHeight: 260, overflow: "auto" }}>
          <input
            className="inp"
            placeholder="Фильтр"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            onKeyDown={(e) => e.stopPropagation()}
          />
          {(["checksum", "crc", "crypto", "fast", "fuzzy"] as const).map((g) => (
            <div key={g}>
              <div className="pgh">{GROUPS[g]}</div>
              {shown
                .filter((a) => a.group === g)
                .map((a) => (
                  <label key={a.id} className="check" style={{ padding: "3px 8px" }}>
                    <input
                      type="checkbox"
                      checked={chosen.includes(a.id)}
                      onChange={(e) =>
                        set({
                          hashAlgos: e.target.checked
                            ? [...chosen, a.id]
                            : chosen.filter((x) => x !== a.id),
                        })
                      }
                    />
                    {a.name}
                  </label>
                ))}
            </div>
          ))}
          <label className="check" style={{ padding: "6px 8px" }}>
            <input
              type="checkbox"
              checked={useCustom}
              onChange={(e) => setUseCustom(e.target.checked)}
            />
            CRC со своими параметрами
          </label>
          {useCustom && (
            <div
              style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 6, padding: "0 8px" }}
            >
              {(["width", "poly", "init", "xorout"] as const).map((k) => (
                <label key={k} className="note">
                  {k}
                  <input
                    className="inp"
                    value={k === "width" ? custom[k] : `0x${custom[k].toString(16).toUpperCase()}`}
                    onKeyDown={(e) => e.stopPropagation()}
                    onChange={(e) => {
                      const v = e.target.value.trim();
                      const n =
                        k === "width"
                          ? Number.parseInt(v, 10)
                          : Number.parseInt(v.replace(/^0x/i, ""), 16);
                      if (Number.isFinite(n)) setCustom({ ...custom, [k]: n });
                    }}
                  />
                </label>
              ))}
              <label className="check">
                <input
                  type="checkbox"
                  checked={custom.refin}
                  onChange={(e) => setCustom({ ...custom, refin: e.target.checked })}
                />
                refin
              </label>
              <label className="check">
                <input
                  type="checkbox"
                  checked={custom.refout}
                  onChange={(e) => setCustom({ ...custom, refout: e.target.checked })}
                />
                refout
              </label>
            </div>
          )}
        </div>
      )}
      <div style={{ display: "flex", gap: 6, padding: "0 4px" }}>
        <button type="button" className={`btn${job ? "" : " pri"}`} onClick={run}>
          {job ? `Остановить · ${Math.round(progress * 100)}%` : "Посчитать"}
        </button>
        <input
          className="inp"
          placeholder="Сравнить с ожидаемым…"
          value={expected}
          onChange={(e) => setExpected(e.target.value)}
          onKeyDown={(e) => e.stopPropagation()}
        />
      </div>
      {job && (
        <div className="progress" style={{ width: "100%" }}>
          <span style={{ width: `${progress * 100}%` }} />
        </div>
      )}
      <div className="list">
        {results.map((r, i) => {
          const match = exp && r.value.toLowerCase() === exp;
          return (
            <button
              type="button"
              key={r.id}
              className={`li${match ? " cur" : ""}`}
              style={{
                ["--delay" as string]: `${i * 0.03}s`,
                gridTemplateColumns: "14px minmax(0,1fr) auto",
              }}
              title="Нажмите, чтобы скопировать"
              onClick={() => writeText(r.value).then(() => toast(`${nameOf(r.id)} скопирован`))}
            >
              {match ? <Icon.check /> : <span />}
              <span className="t">
                <b>{nameOf(r.id)}</b>
                <span style={{ whiteSpace: "normal", wordBreak: "break-all", userSelect: "text" }}>
                  {r.value}
                </span>
              </span>
              <Icon.copy />
            </button>
          );
        })}
      </div>
      {results.length === 0 && !job && (
        <div className="empty">
          <span>
            {has ? "Хэш считается для выделения." : "Без выделения считается хэш всего файла."}{" "}
            Расчёт идёт в фоне по снимку и не мешает правкам.
          </span>
        </div>
      )}
    </>
  );
}
