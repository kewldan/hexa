// Process memory (PROC-02…08): regions and modules, value scanner with
// freezing, pointer scan and dumps for the active process document.
import { save as saveFile } from "@tauri-apps/plugin-dialog";
import { useEffect, useMemo, useState } from "react";
import { cacheFor } from "../../grid/cache";
import {
  type ChainView,
  type DumpWhat,
  type FirstScan,
  type FrozenView,
  type MemMap,
  mem,
  type NextScan,
  type ScanPage,
  type ValueType,
} from "../../ipc/sources";
import { fmtSize, groupDec } from "../../lib/format";
import * as A from "../../state/actions";
import { type Tab, useTabs } from "../../state/tabs";
import { openDialog, toast } from "../../state/ui";
import { addr, isMemorySource, JobBar, parseAddr, useJob, useSourceInfo } from "../sourceKit";

type Section = "regions" | "modules" | "scan" | "pointers";

const TYPES: ValueType[] = ["i32", "u32", "i64", "u64", "f32", "f64", "i16", "u16", "i8", "u8"];
const SIZE: Record<ValueType, number> = {
  u8: 1,
  i8: 1,
  u16: 2,
  i16: 2,
  u32: 4,
  i32: 4,
  f32: 4,
  u64: 8,
  i64: 8,
  f64: 8,
};

const NEXT: { id: NextScan["kind"]; name: string; args: 0 | 1 | 2 }[] = [
  { id: "equal", name: "равно", args: 1 },
  { id: "notEqual", name: "не равно", args: 1 },
  { id: "between", name: "между", args: 2 },
  { id: "changed", name: "изменилось", args: 0 },
  { id: "unchanged", name: "не изменилось", args: 0 },
  { id: "increased", name: "увеличилось", args: 0 },
  { id: "decreased", name: "уменьшилось", args: 0 },
  { id: "increasedBy", name: "увеличилось на", args: 1 },
  { id: "decreasedBy", name: "уменьшилось на", args: 1 },
];

/** Documents whose unmapped gaps were folded once already. */
const folded = new Set<number>();

function perms(p: { read: boolean; write: boolean; execute: boolean }) {
  return `${p.read ? "r" : "-"}${p.write ? "w" : "-"}${p.execute ? "x" : "-"}`;
}

function foldGaps(tab: Tab, map: MemMap) {
  const folds = map.gaps.filter(([a, b]) => b - a >= 4096).map(([start, end]) => ({ start, end }));
  useTabs.getState().update(tab.id, { folds });
}

async function dump(tab: Tab, what: DumpWhat, name: string, job: ReturnType<typeof useJob>) {
  const path = await saveFile({ defaultPath: `${name}.bin` });
  if (!path) return;
  try {
    const r = await job.run((p, j) => mem.dump(tab.id, what, path, p, j));
    if (r)
      toast(
        `Сохранено ${fmtSize(r.bytesWritten)}${
          r.unreadable.length
            ? ` · нечитаемых участков: ${r.unreadable.length} (заполнены нулями)`
            : ""
        }`,
      );
  } catch (e) {
    A.report(e);
  }
}

export function MemoryPanel({ tab }: { tab: Tab }) {
  const info = useSourceInfo(tab.id);
  const [section, setSection] = useState<Section>("regions");
  const [map, setMap] = useState<MemMap | null>(null);
  const [live, setLive] = useState(false);
  const job = useJob();
  const isMem = isMemorySource(info);

  const load = (refresh = false) =>
    mem
      .map(tab.id, refresh)
      .then((m) => {
        setMap(m);
        if (!folded.has(tab.id)) {
          folded.add(tab.id);
          foldGaps(tab, m);
        }
      })
      .catch(A.report);

  useEffect(() => {
    setMap(null);
    if (isMem) void load();
  }, [tab.id, isMem]);

  // Live view (PROC-05): process memory is never cached, so re-reading the
  // visible pages shows current values.
  useEffect(() => {
    if (!live || !isMem) return;
    const t = setInterval(() => cacheFor(tab.id).clear(), 500);
    return () => clearInterval(t);
  }, [live, isMem, tab.id]);

  if (!isMem)
    return (
      <div className="empty">
        <b>Память процесса</b>
        <span>
          Откройте процесс или GDB-сервер — здесь появятся регионы, модули, сканер значений,
          заморозка и поиск указателей.
        </span>
        <button
          type="button"
          className="chip"
          onClick={() => openDialog({ kind: "src", name: "process" })}
        >
          Открыть процесс…
        </button>
      </div>
    );

  return (
    <>
      <div className="ph">
        <span className="cap">Память</span>
        {info?.pid != null && <span className="tag">PID {info.pid}</span>}
        <div className="grow" />
        <button
          type="button"
          className={`chip${live ? " on" : ""}`}
          onClick={() => setLive(!live)}
          title="Перечитывать видимые байты дважды в секунду"
        >
          Живое
        </button>
        <button type="button" className="chip" onClick={() => load(true)} title="Обновить карту">
          ↻
        </button>
      </div>
      <div className="src-tabs" style={{ padding: "0 4px" }} role="tablist">
        {(
          [
            ["regions", "Регионы"],
            ["modules", "Модули"],
            ["scan", "Сканер"],
            ["pointers", "Указатели"],
          ] as const
        ).map(([k, t]) => (
          <button
            type="button"
            role="tab"
            aria-selected={section === k}
            key={k}
            className={`chip${section === k ? " on" : ""}`}
            onClick={() => setSection(k)}
          >
            {t}
          </button>
        ))}
      </div>
      <JobBar job={job} label="Дамп" />
      {section === "regions" && <Regions tab={tab} map={map} job={job} />}
      {section === "modules" && <Modules tab={tab} map={map} job={job} />}
      {section === "scan" && <Scanner tab={tab} writable={!!info?.writable} />}
      {section === "pointers" && <Pointers tab={tab} />}
    </>
  );
}

function Regions({
  tab,
  map,
  job,
}: {
  tab: Tab;
  map: MemMap | null;
  job: ReturnType<typeof useJob>;
}) {
  const [q, setQ] = useState("");
  const list = useMemo(() => {
    const f = q.trim().toLowerCase();
    return (map?.regions ?? []).filter(
      (r) =>
        !f ||
        (r.path ?? r.label ?? "").toLowerCase().includes(f) ||
        r.kind.includes(f) ||
        perms(r.protection).includes(f) ||
        r.start.toString(16).includes(f.replace(/^0x/, "")),
    );
  }, [map, q]);
  if (!map)
    return (
      <div className="note" style={{ padding: "0 4px" }}>
        Читаю карту памяти…
      </div>
    );
  return (
    <div className="src-section">
      <div className="src-inline">
        <input
          className="inp"
          placeholder="Фильтр: путь, rwx, heap, адрес"
          aria-label="Фильтр регионов"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => e.stopPropagation()}
        />
        <button
          type="button"
          className="chip"
          title="Свернуть неотображённые участки адресного пространства"
          onClick={() => foldGaps(tab, map)}
        >
          Свернуть пустоты
        </button>
      </div>
      <div className="note">
        Регионов {groupDec(map.regions.length)} · указатель {map.pointerSize * 8} бит
      </div>
      <div className="list src-tree">
        {list.slice(0, 3000).map((r) => {
          const cur = tab.cursor >= r.start && tab.cursor < r.start + r.size;
          return (
            <div key={r.start} className={`src-row${cur ? " cur" : ""}`}>
              <button
                type="button"
                className="t"
                style={{ textAlign: "left" }}
                onClick={() => A.jumpTo(r.start)}
                title={r.path ?? undefined}
              >
                <b className="mono">
                  {addr(r.start)} <span className="src-perm">{perms(r.protection)}</span>
                </b>
                <span>
                  {r.kind}
                  {r.label || r.path ? ` · ${r.label ?? r.path?.split(/[\\/]/).pop()}` : ""} ·{" "}
                  {fmtSize(r.size)}
                </span>
              </button>
              <button
                type="button"
                className="chip"
                disabled={job.running}
                onClick={() =>
                  dump(tab, { kind: "range", start: r.start, len: r.size }, addr(r.start), job)
                }
              >
                Дамп
              </button>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function Modules({
  tab,
  map,
  job,
}: {
  tab: Tab;
  map: MemMap | null;
  job: ReturnType<typeof useJob>;
}) {
  if (!map) return null;
  if (!map.modules.length) return <div className="note">Модулей не найдено.</div>;
  return (
    <div className="list src-tree">
      {map.modules.map((m) => (
        <div key={`${m.base}:${m.path}`} className="src-row">
          <button
            type="button"
            className="t"
            style={{ textAlign: "left" }}
            onClick={() => A.jumpTo(m.base)}
            title={m.path}
          >
            <b>{m.name}</b>
            <span>
              {addr(m.base)} · {fmtSize(m.size)}
            </span>
          </button>
          <button
            type="button"
            className="chip"
            disabled={job.running}
            onClick={() => dump(tab, { kind: "module", name: m.path }, m.name, job)}
          >
            Дамп
          </button>
        </div>
      ))}
    </div>
  );
}

const PAGE_SIZE = 100;

function Scanner({ tab, writable }: { tab: Tab; writable: boolean }) {
  const [ty, setTy] = useState<ValueType>("i32");
  const [first, setFirst] = useState<FirstScan["kind"]>("exact");
  const [next, setNext] = useState<NextScan["kind"]>("equal");
  const [v1, setV1] = useState("");
  const [v2, setV2] = useState("");
  const [writableOnly, setWritableOnly] = useState(true);
  const [page, setPage] = useState<ScanPage | null>(null);
  const [start, setStart] = useState(0);
  const [frozen, setFrozen] = useState<FrozenView[]>([]);
  const [edit, setEdit] = useState<{ address: number; text: string } | null>(null);
  const job = useJob();
  const started = (page?.scans ?? 0) > 0;
  const curTy = page?.valueType ?? ty;

  const refresh = () => {
    mem
      .scanPage(tab.id, start, PAGE_SIZE, true)
      .then(setPage)
      .catch(() => {});
    if (writable)
      mem
        .frozen(tab.id)
        .then(setFrozen)
        .catch(() => {});
  };
  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 1000);
    return () => clearInterval(t);
  }, [tab.id, start]);

  const runFirst = async () => {
    const condition: FirstScan =
      first === "exact"
        ? { kind: "exact", value: v1 }
        : first === "range"
          ? { kind: "range", min: v1, max: v2 }
          : { kind: "unknown" };
    try {
      const r = await job.run((p, j) =>
        mem.scanFirst(
          tab.id,
          {
            valueType: ty,
            regions: { writableOnly, includeExecutable: true, includeMapped: true },
          },
          condition,
          p,
          j,
        ),
      );
      if (r) toast(`Найдено: ${groupDec(r.count)} · прочитано ${fmtSize(r.scannedBytes)}`);
      setStart(0);
      refresh();
    } catch (e) {
      A.report(e);
    }
  };
  const runNext = async () => {
    const args = NEXT.find((n) => n.id === next)?.args ?? 0;
    const condition = (
      args === 0
        ? { kind: next }
        : args === 1
          ? { kind: next, value: v1 }
          : { kind: next, min: v1, max: v2 }
    ) as NextScan;
    try {
      const r = await job.run((p, j) => mem.scanNext(tab.id, condition, p, j));
      if (r) toast(`Осталось: ${groupDec(r.count)}`);
      setStart(0);
      refresh();
    } catch (e) {
      A.report(e);
    }
  };
  const reset = () =>
    mem
      .scanReset(tab.id)
      .then(() => {
        setPage(null);
        setStart(0);
      })
      .catch(A.report);
  const write = async (address: number, text: string) => {
    try {
      await mem.writeValue(tab.id, address, curTy, text);
      cacheFor(tab.id).clear();
      refresh();
    } catch (e) {
      A.report(e);
    }
  };
  const freeze = async (address: number, value: string) => {
    try {
      await mem.freeze(tab.id, address, curTy, value);
      refresh();
    } catch (e) {
      A.report(e);
    }
  };
  const nArgs = started
    ? (NEXT.find((n) => n.id === next)?.args ?? 0)
    : first === "range"
      ? 2
      : first === "unknown"
        ? 0
        : 1;
  const stop = (e: React.KeyboardEvent) => e.stopPropagation();

  return (
    <div className="src-section">
      <div className="src-inline">
        <select
          className="sel-input"
          aria-label="Тип значения"
          value={curTy}
          disabled={started}
          onChange={(e) => setTy(e.target.value as ValueType)}
        >
          {TYPES.map((t) => (
            <option key={t} value={t}>
              {t}
            </option>
          ))}
        </select>
        {started ? (
          <select
            className="sel-input"
            aria-label="Условие"
            value={next}
            onChange={(e) => setNext(e.target.value as NextScan["kind"])}
          >
            {NEXT.map((n) => (
              <option key={n.id} value={n.id}>
                {n.name}
              </option>
            ))}
          </select>
        ) : (
          <select
            className="sel-input"
            aria-label="Режим первого сканирования"
            value={first}
            onChange={(e) => setFirst(e.target.value as FirstScan["kind"])}
          >
            <option value="exact">точное значение</option>
            <option value="range">диапазон</option>
            <option value="unknown">неизвестное</option>
          </select>
        )}
      </div>
      {nArgs > 0 && (
        <div className="src-inline">
          <input
            className="inp"
            aria-label="Значение"
            placeholder={nArgs === 2 ? "от" : "значение (10, 0x1F, 1.5)"}
            value={v1}
            onChange={(e) => setV1(e.target.value)}
            onKeyDown={stop}
          />
          {nArgs === 2 && (
            <input
              className="inp"
              aria-label="Верхняя граница"
              placeholder="до"
              value={v2}
              onChange={(e) => setV2(e.target.value)}
              onKeyDown={stop}
            />
          )}
        </div>
      )}
      <div className="src-inline">
        {!started && (
          <label className="chk" style={{ margin: 0 }}>
            <input
              type="checkbox"
              checked={writableOnly}
              onChange={(e) => setWritableOnly(e.target.checked)}
            />
            только записываемые
          </label>
        )}
        <div className="grow" />
        {started && (
          <button type="button" className="chip" onClick={reset} disabled={job.running}>
            Сброс
          </button>
        )}
        <button
          type="button"
          className="chip on"
          disabled={job.running || (nArgs > 0 && !v1.trim())}
          onClick={started ? runNext : runFirst}
        >
          {started ? "Следующее" : "Первое сканирование"}
        </button>
      </div>
      <JobBar job={job} label="Сканирование" />
      {page && page.count > 0 && (
        <>
          <div className="src-inline note">
            <span>
              Результатов {groupDec(page.count)} · сканирований {page.scans}
            </span>
            <div className="grow" />
            <button
              type="button"
              className="chip"
              disabled={start === 0}
              onClick={() => setStart(Math.max(0, start - PAGE_SIZE))}
            >
              ←
            </button>
            <span>
              {start + 1}–{Math.min(start + PAGE_SIZE, page.count)}
            </span>
            <button
              type="button"
              className="chip"
              disabled={start + PAGE_SIZE >= page.count}
              onClick={() => setStart(start + PAGE_SIZE)}
            >
              →
            </button>
          </div>
          <div className="list src-hits" role="list" aria-label="Результаты сканирования">
            {page.hits.map((h) => (
              <div key={h.address} className="src-hit">
                <button
                  type="button"
                  className="mono"
                  style={{ textAlign: "left" }}
                  onClick={() => A.jumpTo(h.address, SIZE[curTy])}
                >
                  {addr(h.address)}
                </button>
                {edit?.address === h.address ? (
                  <input
                    className="inp"
                    aria-label="Новое значение"
                    // biome-ignore lint/a11y/noAutofocus: inline editor opened by the user
                    autoFocus
                    value={edit.text}
                    onChange={(e) => setEdit({ address: h.address, text: e.target.value })}
                    onBlur={() => setEdit(null)}
                    onKeyDown={(e) => {
                      e.stopPropagation();
                      if (e.key === "Enter") {
                        void write(h.address, edit.text);
                        setEdit(null);
                      } else if (e.key === "Escape") setEdit(null);
                    }}
                  />
                ) : (
                  <button
                    type="button"
                    style={{ textAlign: "left" }}
                    className={h.current !== null && h.current !== h.value ? "chg" : ""}
                    title={`При сканировании: ${h.value}${writable ? " · клик — изменить" : ""}`}
                    disabled={!writable}
                    onClick={() => setEdit({ address: h.address, text: h.current ?? h.value })}
                  >
                    {h.current ?? "??"}
                  </button>
                )}
                {writable && (
                  <button
                    type="button"
                    className="chip"
                    title="Заморозить текущее значение"
                    onClick={() => freeze(h.address, h.current ?? h.value)}
                  >
                    ❄
                  </button>
                )}
              </div>
            ))}
          </div>
        </>
      )}
      {page && started && page.count === 0 && <div className="note">Совпадений не осталось.</div>}
      {!writable && (
        <div className="note">Процесс открыт только для чтения: запись и заморозка недоступны.</div>
      )}
      {frozen.length > 0 && (
        <>
          <span className="cap">Заморожено</span>
          <div className="list">
            {frozen.map((f) => (
              <div key={f.id} className="src-hit">
                <button
                  type="button"
                  className="mono"
                  style={{ textAlign: "left" }}
                  onClick={() => A.jumpTo(f.address)}
                  title={f.lastError ?? `записей: ${f.writes}`}
                >
                  {addr(f.address)}
                </button>
                <span className={f.lastError ? "chg" : ""}>
                  {f.value} {f.valueType ? `(${f.valueType})` : ""}
                </span>
                <span className="src-inline">
                  <button
                    type="button"
                    className={`chip${f.enabled ? " on" : ""}`}
                    onClick={() =>
                      mem
                        .freezeControl(tab.id, f.id, f.enabled ? "disable" : "enable")
                        .then(refresh)
                        .catch(A.report)
                    }
                  >
                    {f.enabled ? "вкл" : "выкл"}
                  </button>
                  <button
                    type="button"
                    className="chip"
                    aria-label="Разморозить"
                    onClick={() =>
                      mem.freezeControl(tab.id, f.id, "remove").then(refresh).catch(A.report)
                    }
                  >
                    ×
                  </button>
                </span>
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

function Pointers({ tab }: { tab: Tab }) {
  const [target, setTarget] = useState(addr(tab.cursor));
  const [depth, setDepth] = useState("3");
  const [maxOffset, setMaxOffset] = useState("0x1000");
  const [chains, setChains] = useState<{ total: number; chains: ChainView[] } | null>(null);
  const job = useJob();
  const load = () =>
    mem
      .pointerResults(tab.id, 0, 200)
      .then(setChains)
      .catch(() => {});
  useEffect(() => {
    void load();
  }, [tab.id]);
  const t = parseAddr(target);
  const run = async () => {
    if (t === null) return;
    try {
      const r = await job.run((p, j) =>
        mem.pointerScan(
          tab.id,
          t,
          {
            maxDepth: Math.min(5, Math.max(1, Number(depth) || 3)),
            maxOffset: parseAddr(maxOffset) ?? 0x1000,
            maxResults: 10_000,
          },
          p,
          j,
        ),
      );
      if (r) toast(`Цепочек: ${groupDec(r.count)}`);
      await load();
    } catch (e) {
      A.report(e);
    }
  };
  const rescan = async () => {
    if (t === null) return;
    try {
      const n = await mem.pointerRescan(tab.id, t);
      toast(`Осталось цепочек: ${groupDec(n)}`);
      await load();
    } catch (e) {
      A.report(e);
    }
  };
  const stop = (e: React.KeyboardEvent) => e.stopPropagation();
  return (
    <div className="src-section">
      <div className="src-inline">
        <input
          className={`inp${t === null ? " bad" : ""}`}
          aria-label="Целевой адрес"
          value={target}
          onChange={(e) => setTarget(e.target.value)}
          onKeyDown={stop}
        />
        <button
          type="button"
          className="chip"
          title="Адрес под курсором"
          onClick={() => setTarget(addr(tab.cursor))}
        >
          Курсор
        </button>
      </div>
      <div className="src-inline">
        <input
          className="inp"
          aria-label="Глубина"
          title="Глубина (1–5)"
          value={depth}
          onChange={(e) => setDepth(e.target.value)}
          onKeyDown={stop}
        />
        <input
          className="inp"
          aria-label="Максимальное смещение"
          title="Максимальное смещение"
          value={maxOffset}
          onChange={(e) => setMaxOffset(e.target.value)}
          onKeyDown={stop}
        />
      </div>
      <div className="src-inline">
        <div className="grow" />
        {chains && chains.total > 0 && (
          <button
            type="button"
            className="chip"
            disabled={job.running || t === null}
            onClick={rescan}
            title="Оставить цепочки, которые теперь ведут к этому адресу"
          >
            Пересканировать
          </button>
        )}
        <button
          type="button"
          className="chip on"
          disabled={job.running || t === null}
          onClick={run}
        >
          Найти цепочки
        </button>
      </div>
      <JobBar job={job} label="Поиск указателей" />
      {chains && chains.total > 0 && (
        <>
          <div className="note">
            Цепочек {groupDec(chains.total)}
            {chains.total > chains.chains.length ? `, показаны первые ${chains.chains.length}` : ""}
          </div>
          <div className="list src-hits">
            {chains.chains.map((c) => (
              <button
                type="button"
                key={c.text}
                className="src-row"
                disabled={c.resolved === null}
                onClick={() => c.resolved !== null && A.jumpTo(c.resolved)}
              >
                <span className="t">
                  <b className="mono">{c.text}</b>
                  <span>{c.resolved !== null ? `→ ${addr(c.resolved)}` : "не разрешается"}</span>
                </span>
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
