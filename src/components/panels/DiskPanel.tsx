// Partition table, file systems and the sector → owner map of the active
// disk, image or file (DISK-04…07).
import { save as saveFile } from "@tauri-apps/plugin-dialog";
import { useEffect, useRef, useState } from "react";
import {
  type CarvedFile,
  type DiskLayout,
  type FileEntry,
  type OwnerInfo,
  src,
  type VolumeView,
} from "../../ipc/sources";
import { fmtSize } from "../../lib/format";
import * as A from "../../state/actions";
import { type Tab, useTabs } from "../../state/tabs";
import { openDialog, toast } from "../../state/ui";
import { addr, isMemorySource, JobBar, useJob, useSourceInfo } from "../sourceKit";

const OWNER_KIND: Record<string, string> = {
  metadata: "служебные данные",
  file: "файл",
  directory: "каталог",
};

/** Maps already built (or being built) per document and volume. */
const mapped = new Set<string>();

export function DiskPanel({ tab }: { tab: Tab }) {
  const info = useSourceInfo(tab.id);
  const [layout, setLayout] = useState<DiskLayout | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [vol, setVol] = useState<VolumeView | null>(null);
  const memory = isMemorySource(info);

  useEffect(() => {
    if (info === undefined || memory) return;
    let alive = true;
    setLayout(null);
    setError(null);
    setVol(null);
    src
      .layout(tab.id)
      .then((l) => {
        if (!alive) return;
        setLayout(l);
        setVol(l.volumes.find((v) => v.browsable) ?? null);
      })
      .catch((e) => alive && setError(e?.message ?? String(e)));
    return () => {
      alive = false;
    };
  }, [tab.id, info === undefined, memory]);

  if (memory)
    return (
      <div className="empty">
        <span>Для памяти процесса используйте панель «Память».</span>
      </div>
    );

  return (
    <>
      <div className="ph">
        <span className="cap">Диск</span>
        {layout && <span className="tag">{fmtSize(layout.size)}</span>}
        <div className="grow" />
        <button
          type="button"
          className="chip"
          title="Клонировать или снять образ (raw / E01)"
          onClick={() => openDialog({ kind: "src", name: "clone" })}
        >
          Образ…
        </button>
        {info?.writable && (
          <button
            type="button"
            className="chip"
            onClick={() => openDialog({ kind: "src", name: "commit" })}
          >
            Записать…
          </button>
        )}
      </div>
      {info && (
        <div className="note" style={{ padding: "0 4px" }}>
          {info.label} · {info.writable ? "запись разрешена" : "только чтение"}
        </div>
      )}
      {error && (
        <div className="empty">
          <span>{error}</span>
        </div>
      )}
      {!layout && !error && (
        <div className="note" style={{ padding: "0 4px" }}>
          Читаю разметку…
        </div>
      )}
      {layout && (
        <Partitions
          tab={tab}
          layout={layout}
          vol={vol}
          onPick={(v) => {
            setVol(v.browsable ? v : null);
            A.selectRange(v.offset, v.offset + Math.max(1, v.size));
          }}
        />
      )}
      {vol && <Owner tab={tab} vol={vol} />}
      {vol && <FsTree key={`${tab.id}:${vol.index}`} tab={tab} vol={vol} />}
      <Carver tab={tab} />
    </>
  );
}

function Partitions({
  tab,
  layout,
  vol,
  onPick,
}: {
  tab: Tab;
  layout: DiskLayout;
  vol: VolumeView | null;
  onPick: (v: VolumeView) => void;
}) {
  const t = layout.table;
  return (
    <div className="src-section">
      <div className="note">
        {t
          ? `${t.kind.toUpperCase()} · сектор ${t.sectorSize} Б · разделов ${t.partitions.length}`
          : layout.volumes[0]?.fs
            ? "Без таблицы разделов — файловая система на всём документе"
            : "Таблица разделов и файловая система не найдены"}
      </div>
      {t?.warnings.map((w) => (
        <div key={w} className="src-warn">
          {w}
        </div>
      ))}
      <div className="list">
        {layout.volumes
          .filter((v) => t || v.fs)
          .map((v) => {
            const part = v.index != null ? t?.partitions[v.index] : undefined;
            const inside = tab.cursor >= v.offset && tab.cursor < v.offset + v.size;
            return (
              <button
                type="button"
                key={v.index ?? "all"}
                className={`src-row${vol?.index === v.index && vol?.offset === v.offset ? " cur" : ""}`}
                onClick={() => onPick(v)}
                title={part ? `${part.typeName} · ${part.typeId}` : undefined}
              >
                <span className="t">
                  <b>
                    {inside ? "▸ " : ""}
                    {v.name}
                    {part?.bootable ? " · загрузочный" : ""}
                  </b>
                  <span>
                    {addr(v.offset)} ·{" "}
                    {v.fs
                      ? `${v.fs.kind}${v.fs.label ? ` «${v.fs.label}»` : ""}${v.browsable ? "" : " (только распознана)"}`
                      : (part?.typeName ?? "нет ФС")}
                  </span>
                </span>
                <span className="v">{fmtSize(v.size)}</span>
              </button>
            );
          })}
      </div>
    </div>
  );
}

function Owner({ tab, vol }: { tab: Tab; vol: VolumeView }) {
  const [owner, setOwner] = useState<OwnerInfo | null>(null);
  const [building, setBuilding] = useState(false);
  const [ready, setReady] = useState(0);
  const key = `${tab.id}:${vol.index}`;
  useEffect(() => {
    if (mapped.has(key)) return;
    mapped.add(key);
    setBuilding(true);
    src
      .fsMapBuild(tab.id, vol.index)
      .then(() => setReady((n) => n + 1))
      .catch((e) => {
        mapped.delete(key);
        A.report(e);
      })
      .finally(() => setBuilding(false));
  }, [key]);
  useEffect(() => {
    const t = setTimeout(() => {
      src
        .fsOwnerAt(tab.id, tab.cursor)
        .then(setOwner)
        .catch(() => setOwner(null));
    }, 120);
    return () => clearTimeout(t);
  }, [tab.id, tab.cursor, ready]);
  const sector = Math.floor(tab.cursor / 512);
  return (
    <div className="src-section">
      <div className="src-owner" role="status">
        <span className="note">Сектор {sector} принадлежит: </span>
        {owner?.region ? (
          <button
            type="button"
            className="chip"
            style={{ maxWidth: "100%" }}
            onClick={() => {
              const r = owner.region;
              if (r) A.selectRange(r.start, r.start + Math.max(1, r.len));
            }}
          >
            {OWNER_KIND[owner.region.kind] ?? owner.region.kind}: {owner.region.label}
          </button>
        ) : owner ? (
          <b>
            {owner.mapped ? "свободное место" : building ? "карта строится…" : "—"} ·{" "}
            {owner.volumeName}
          </b>
        ) : (
          <b>вне тома</b>
        )}
        {owner?.detail && <div className="note">{owner.detail}</div>}
      </div>
    </div>
  );
}

function FsTree({ tab, vol }: { tab: Tab; vol: VolumeView }) {
  const [stack, setStack] = useState<FileEntry[]>([]);
  const [entries, setEntries] = useState<FileEntry[] | null>(null);
  const [deleted, setDeleted] = useState(false);
  const [cur, setCur] = useState<FileEntry | null>(null);
  const job = useJob();
  const dir = stack[stack.length - 1] ?? null;
  const seq = useRef(0);
  useEffect(() => {
    const n = ++seq.current;
    setEntries(null);
    const p = deleted
      ? src.fsDeleted(tab.id, vol.index)
      : src.fsList(tab.id, vol.index, dir).then((r) => r.entries);
    p.then((e) => n === seq.current && setEntries(e)).catch((e) => {
      if (n === seq.current) setEntries([]);
      A.report(e);
    });
  }, [dir?.id, dir?.path, deleted]);

  const select = async (f: FileEntry) => {
    setCur(f);
    try {
      const ext = (await src.fsExtents(tab.id, vol.index, f)).filter((e) => e.physical != null);
      if (!ext.length) return toast(f.size ? "У файла нет размещённых блоков" : "Пустой файл");
      const [first, ...rest] = ext;
      const s = first.physical ?? 0;
      A.selectRange(s, s + first.len);
      useTabs.getState().update(tab.id, {
        extra: rest.map((e) => ({ start: e.physical ?? 0, end: (e.physical ?? 0) + e.len })),
      });
      if (rest.length) toast(`Фрагментов: ${ext.length} — все выделены`);
    } catch (e) {
      A.report(e);
    }
  };
  const openFile = (f: FileEntry) =>
    src
      .fsOpenFile(tab.id, vol.index, f)
      .then((i) => A.adopt(i))
      .catch(A.report);
  const extract = async (f: FileEntry) => {
    const dest = await saveFile({ defaultPath: f.name });
    if (!dest) return;
    try {
      const r = await job.run((p) => src.fsExtract(tab.id, vol.index, f, dest, p));
      if (r) toast(`Извлечено ${fmtSize(f.size)} · MD5 ${r.md5}`);
    } catch (e) {
      A.report(e);
    }
  };

  return (
    <div className="src-section">
      <div className="src-inline">
        <span className="cap">Файлы · {vol.fs?.kind}</span>
        <div className="grow" />
        <button
          type="button"
          className={`chip${deleted ? " on" : ""}`}
          onClick={() => setDeleted(!deleted)}
          title="Удалённые записи, которые ещё можно найти (DISK-07)"
        >
          Удалённые
        </button>
      </div>
      {!deleted && stack.length > 0 && (
        <div className="src-inline note">
          <button type="button" className="chip" onClick={() => setStack(stack.slice(0, -1))}>
            ← Назад
          </button>
          <span className="mono" style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
            {dir?.path}
          </span>
        </div>
      )}
      {entries === null && <div className="note">Читаю…</div>}
      {entries?.length === 0 && (
        <div className="note">{deleted ? "Удалённых записей не найдено." : "Каталог пуст."}</div>
      )}
      <div className="list src-tree">
        {entries?.slice(0, 3000).map((f) => (
          <div
            key={`${f.id}:${f.aux}:${f.path}`}
            className={`src-row${cur?.path === f.path && cur.id === f.id ? " cur" : ""}`}
          >
            <button
              type="button"
              className="t"
              style={{ textAlign: "left" }}
              onClick={() => (f.isDir && !deleted ? setStack([...stack, f]) : select(f))}
              onDoubleClick={() => !f.isDir && openFile(f)}
              title={f.isDir ? "Открыть каталог" : "Клик — выделить блоки, двойной — открыть"}
            >
              <b style={f.deleted ? { color: "var(--mod)" } : undefined}>
                {f.isDir ? "▸ " : ""}
                {f.name}
              </b>
              <span>
                {f.isDir ? "каталог" : fmtSize(f.size)}
                {f.modified ? ` · ${new Date(f.modified * 1000).toLocaleDateString()}` : ""}
              </span>
            </button>
            {!f.isDir && (
              <span className="src-inline">
                <button type="button" className="chip" onClick={() => openFile(f)}>
                  Открыть
                </button>
                <button type="button" className="chip" onClick={() => extract(f)}>
                  Извлечь…
                </button>
              </span>
            )}
          </div>
        ))}
      </div>
      <JobBar job={job} label="Извлечение" />
    </div>
  );
}

function Carver({ tab }: { tab: Tab }) {
  const [found, setFound] = useState<CarvedFile[] | null>(null);
  const job = useJob();
  const run = async () => {
    try {
      const r = await job.run((p, j) => src.carve(tab.id, 0, null, 512, p, j));
      if (r) {
        setFound(r);
        toast(`Найдено сигнатур: ${r.length}`);
      }
    } catch (e) {
      A.report(e);
    }
  };
  return (
    <div className="src-section">
      <div className="src-inline">
        <span className="cap">Карвинг</span>
        <div className="grow" />
        <button
          type="button"
          className="chip"
          disabled={job.running}
          onClick={run}
          title="Поиск файлов по сигнатурам на границах секторов"
        >
          Искать файлы
        </button>
      </div>
      <JobBar job={job} label="Карвинг" />
      {found && found.length === 0 && <div className="note">Ничего не найдено.</div>}
      <div className="list src-tree">
        {found?.slice(0, 2000).map((f) => (
          <button
            type="button"
            key={f.offset}
            className="src-row"
            onClick={() => A.selectRange(f.offset, f.offset + Math.max(1, f.size))}
          >
            <span className="t">
              <b>
                {f.kind} (.{f.extension})
              </b>
              <span>
                {addr(f.offset)}
                {f.complete ? "" : " · размер приблизительный"}
              </span>
            </span>
            <span className="v">{fmtSize(f.size)}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
