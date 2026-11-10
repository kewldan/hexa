// Three-way merge and binary patches (DIFF-09, DIFF-12, IO-05).
import { open as openFile, save as saveFile } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";
import { api, type PatchFormat, type PatchInfo } from "../../ipc/api";
import { fmtSize, hexN } from "../../lib/format";
import * as A from "../../state/actions";
import { type Tab, useActiveTab, useTabs } from "../../state/tabs";
import { closeDialog, toast } from "../../state/ui";
import { Modal } from "./Dialogs";
import "../../styles/tools.css";

const FORMATS: { id: PatchFormat; name: string; ext: string; note: string }[] = [
  { id: "bps", name: "BPS", ext: "bps", note: "компактный, с CRC32, любые размеры" },
  { id: "ups", name: "UPS", ext: "ups", note: "XOR-записи, обратимый" },
  { id: "ips", name: "IPS", ext: "ips", note: "классика ROM-хаков, до 16 МБ" },
  { id: "vcdiff", name: "VCDIFF", ext: "vcdiff", note: "RFC 3284, совместим с xdelta3" },
  { id: "bsdiff", name: "bsdiff", ext: "bsdiff", note: "BSDIFF40, для исполняемых файлов" },
];

function TabSelect({
  label,
  value,
  onChange,
  tabs,
}: {
  label: string;
  value: number;
  onChange: (id: number) => void;
  tabs: Tab[];
}) {
  return (
    <div className="row-f">
      <span>{label}</span>
      <select className="inp" value={value} onChange={(e) => onChange(Number(e.target.value))}>
        {tabs.map((t) => (
          <option key={t.id} value={t.id}>
            {t.info.name} · {fmtSize(t.info.len)}
          </option>
        ))}
      </select>
    </div>
  );
}

export function Merge3Dialog() {
  const tabs = useTabs((s) => s.tabs);
  const cur = useActiveTab();
  const [base, setBase] = useState(tabs[0]?.id ?? 0);
  const [ours, setOurs] = useState(cur?.id ?? tabs[1]?.id ?? 0);
  const [theirs, setTheirs] = useState(tabs[2]?.id ?? tabs[1]?.id ?? 0);
  const [conflict, setConflict] = useState<"ours" | "theirs" | "base">("ours");
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    try {
      const r = await api.merge3(base, ours, theirs, conflict);
      closeDialog();
      await A.adopt(r.info);
      const n = r.result.conflicts.length;
      toast(n ? `Слияние: конфликтов ${n}, они отмечены заметками` : "Слияние без конфликтов");
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title="Трёхстороннее слияние"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            onClick={run}
            disabled={busy || tabs.length < 2}
          >
            Слить
          </button>
        </>
      }
    >
      {tabs.length < 2 ? (
        <div className="note">Откройте общий предок и две изменённые версии.</div>
      ) : (
        <>
          <TabSelect label="Общий предок" value={base} onChange={setBase} tabs={tabs} />
          <TabSelect label="Наша версия" value={ours} onChange={setOurs} tabs={tabs} />
          <TabSelect label="Их версия" value={theirs} onChange={setTheirs} tabs={tabs} />
          <div className="row-f">
            <span>В конфликтах пока взять</span>
            <select
              className="inp"
              value={conflict}
              onChange={(e) => setConflict(e.target.value as typeof conflict)}
            >
              <option value="ours">нашу версию</option>
              <option value="theirs">их версию</option>
              <option value="base">предка</option>
            </select>
          </div>
          <div className="note">
            Результат откроется в новой вкладке, байты не копируются. Конфликты станут красными
            заметками — их видно в панели закладок.
          </div>
        </>
      )}
    </Modal>
  );
}

export function PatchCreateDialog() {
  const tabs = useTabs((s) => s.tabs);
  const cur = useActiveTab();
  const [source, setSource] = useState(tabs.find((t) => t.id !== cur?.id)?.id ?? tabs[0]?.id ?? 0);
  const [target, setTarget] = useState(cur?.id ?? 0);
  const [format, setFormat] = useState<PatchFormat>("bps");
  const [busy, setBusy] = useState(false);
  const run = async () => {
    const f = FORMATS.find((x) => x.id === format) ?? FORMATS[0];
    const name = tabs.find((t) => t.id === target)?.info.name ?? "patch";
    const path = await saveFile({
      defaultPath: `${name}.${f.ext}`,
      filters: [{ name: f.name, extensions: [f.ext] }],
    });
    if (!path) return;
    setBusy(true);
    try {
      const n = await api.patchCreate(source, target, format, path);
      closeDialog();
      toast(`Патч ${f.name} сохранён · ${fmtSize(n)}`);
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title="Создать патч"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            onClick={run}
            disabled={busy || tabs.length < 2 || source === target}
          >
            Сохранить…
          </button>
        </>
      }
    >
      {tabs.length < 2 ? (
        <div className="note">Откройте исходный и изменённый файлы.</div>
      ) : (
        <>
          <TabSelect label="Исходный файл" value={source} onChange={setSource} tabs={tabs} />
          <TabSelect label="Изменённый файл" value={target} onChange={setTarget} tabs={tabs} />
          <div className="opt-list">
            {FORMATS.map((f) => (
              <button
                type="button"
                key={f.id}
                className={`opt${f.id === format ? " on" : ""}`}
                onClick={() => setFormat(f.id)}
              >
                <span />
                <span>
                  <b>{f.name}</b>
                  <span className="note"> · {f.note}</span>
                </span>
                <span />
              </button>
            ))}
          </div>
        </>
      )}
    </Modal>
  );
}

export function PatchApplyDialog({ path: initial }: { path?: string }) {
  const cur = useActiveTab();
  const [path, setPath] = useState(initial ?? "");
  const [info, setInfo] = useState<PatchInfo | null>(null);
  const [verify, setVerify] = useState(true);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!path) return;
    api
      .patchInspect(path)
      .then(setInfo)
      .catch((e) => {
        setInfo(null);
        A.report(e);
      });
  }, [path]);
  const pick = async () => {
    const p = await openFile({
      multiple: false,
      filters: [
        { name: "Патчи", extensions: ["ips", "ups", "bps", "vcdiff", "xdelta", "bsdiff", "patch"] },
      ],
    });
    if (typeof p === "string") setPath(p);
  };
  useEffect(() => {
    if (!initial) void pick();
  }, [initial]);
  const run = async () => {
    if (!cur || !path) return;
    setBusy(true);
    try {
      const r = await api.patchApply(cur.id, path, null, verify);
      closeDialog();
      await A.adopt(r.info);
      toast(
        r.reversed ? "Патч применён в обратную сторону" : `Патч ${r.format.toUpperCase()} применён`,
      );
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  const mismatch = info?.sourceSize != null && cur && info.sourceSize !== cur.info.len;
  return (
    <Modal
      title="Применить патч"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button type="button" className="btn pri" onClick={run} disabled={busy || !info || !cur}>
            Применить
          </button>
        </>
      }
    >
      <div className="row-f">
        <span>Файл патча</span>
        <div style={{ display: "flex", gap: 6 }}>
          <input className="inp" value={path} readOnly />
          <button type="button" className="btn" onClick={pick}>
            Выбрать…
          </button>
        </div>
      </div>
      {info && (
        <div className="kv">
          <span>Формат</span>
          <b>{info.format?.toUpperCase() ?? "?"}</b>
          {info.sourceSize != null && (
            <>
              <span>Исходный размер</span>
              <b className={mismatch ? "bad" : ""}>{fmtSize(info.sourceSize)}</b>
            </>
          )}
          {info.targetSize != null && (
            <>
              <span>Итоговый размер</span>
              <b>{fmtSize(info.targetSize)}</b>
            </>
          )}
          {info.sourceCrc32 != null && (
            <>
              <span>CRC32 исходного</span>
              <b className="mono">{hexN(info.sourceCrc32, 8)}</b>
            </>
          )}
          {info.patchChecksumOk != null && (
            <>
              <span>Контрольная сумма патча</span>
              <b>{info.patchChecksumOk ? "верна" : "НЕ совпадает"}</b>
            </>
          )}
          {info.metadata && (
            <>
              <span>Метаданные</span>
              <b>{info.metadata}</b>
            </>
          )}
        </div>
      )}
      <label className="chk">
        <input type="checkbox" checked={verify} onChange={(e) => setVerify(e.target.checked)} />
        Проверять контрольные суммы
      </label>
      <div className="note">
        Патч применяется к «{cur?.info.name}» с учётом несохранённых правок; результат откроется в
        новой вкладке.
      </div>
    </Modal>
  );
}

type Nway = Awaited<ReturnType<typeof api.diffNway>>;

/** Several files against one base (DIFF-03). */
export function NwayDialog() {
  const tabs = useTabs((s) => s.tabs);
  const cur = useActiveTab();
  const [base, setBase] = useState(cur?.id ?? tabs[0]?.id ?? 0);
  const [picked, setPicked] = useState<number[]>(
    tabs.filter((t) => t.id !== (cur?.id ?? 0)).map((t) => t.id),
  );
  const [res, setRes] = useState<Nway | null>(null);
  const [busy, setBusy] = useState(false);
  const inputs = picked.filter((id) => id !== base);
  const run = async () => {
    setBusy(true);
    try {
      setRes(await api.diffNway(base, inputs));
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  const name = (id: number) => tabs.find((t) => t.id === id)?.info.name ?? "?";
  return (
    <Modal
      title="Сравнить несколько файлов"
      wide
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Закрыть
          </button>
          <button
            type="button"
            className="btn pri"
            onClick={run}
            disabled={busy || inputs.length === 0}
          >
            {busy ? "Сравниваю…" : "Сравнить"}
          </button>
        </>
      }
    >
      <TabSelect label="База" value={base} onChange={setBase} tabs={tabs} />
      <div className="row-f">
        <span>С чем сравнить</span>
        <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
          {tabs
            .filter((t) => t.id !== base)
            .map((t) => (
              <label key={t.id} className="chk" style={{ margin: 0 }}>
                <input
                  type="checkbox"
                  checked={picked.includes(t.id)}
                  onChange={(e) =>
                    setPicked(
                      e.target.checked ? [...picked, t.id] : picked.filter((x) => x !== t.id),
                    )
                  }
                />
                {t.info.name}
              </label>
            ))}
        </div>
      </div>
      {res && (
        <>
          <div className="note">
            Регионов с отличиями: {res.regions.length}
            {res.truncated ? " (список обрезан)" : ""} ·{" "}
            {inputs
              .map((id, i) => `${name(id)}: ${res.stats[i]?.matchPercent.toFixed(1)}%`)
              .join(" · ")}
          </div>
          <div className="nway">
            <div className="nway-row head">
              <span>База</span>
              {inputs.map((id) => (
                <span key={id}>{name(id)}</span>
              ))}
            </div>
            {res.regions.slice(0, 1000).map((r) => (
              <button
                type="button"
                key={r.base.start}
                className="nway-row"
                onClick={() => {
                  useTabs.getState().activate(base);
                  A.selectRange(r.base.start, r.base.start + Math.max(1, r.base.len));
                }}
              >
                <span className="mono">
                  {hexN(r.base.start)}+{r.base.len}
                </span>
                {r.inputs.map((s, i) => (
                  // biome-ignore lint/suspicious/noArrayIndexKey: one cell per input, in order
                  <span key={i} className={`mono k-${s.kind}`}>
                    {s.kind === "equal" ? "=" : `${hexN(s.span.start)}+${s.span.len}`}
                  </span>
                ))}
              </button>
            ))}
          </div>
        </>
      )}
    </Modal>
  );
}
