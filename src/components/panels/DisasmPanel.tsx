// Disassembly at the cursor with inline patching, executable structure,
// cross-references, patch lists and external tools (RE-01…09, ANL-12…14).
import { open as openFile, save as saveFile } from "@tauri-apps/plugin-dialog";
import { useEffect, useMemo, useState } from "react";
import { api } from "../../ipc/api";
import {
  type ArchInfo,
  type ExecutableInfo,
  type ExternalTool,
  type Finding,
  type Instruction,
  type InstructionPatch,
  offsetToVa,
  type PatchList,
  re,
  vaToOffset,
  type XrefReport,
} from "../../ipc/re";
import { fmtSize, hexN } from "../../lib/format";
import * as A from "../../state/actions";
import { useOverlays } from "../../state/overlays";
import { hasSelection, selection, type Tab, useTabs } from "../../state/tabs";
import { toast } from "../../state/ui";
import "../../styles/analysis.css";

type Section = "code" | "file" | "xrefs" | "patches" | "tools";
const PALETTE = ["#2F6BF0", "#1F9D74", "#E8890C", "#7C4DFF", "#D6336C", "#0EA5E9", "#CA8A04"];

const jump = (off: number) => A.setCursor(off, false, { jump: true });

function useExe(tab: Tab) {
  const [info, setInfo] = useState<ExecutableInfo | null>(null);
  const [findings, setFindings] = useState<Finding[]>([]);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    re.info(tab.id)
      .then((r) => {
        if (!live) return;
        setInfo(r.info);
        setFindings(r.findings);
        setError(r.error);
      })
      .catch(() => live && setInfo(null));
    return () => {
      live = false;
    };
  }, [tab.id, tab.info.version]);
  // Sections and headers as a grid layer.
  useEffect(() => {
    const o = useOverlays.getState();
    if (!info) {
      o.clear(tab.id, "exe");
      return;
    }
    const regions = [
      ...info.headers.map((h) => ({
        start: h.offset,
        end: h.offset + h.size,
        color: "#6B6B73",
        label: h.name,
      })),
      ...info.sections
        .filter((s) => s.fileSize > 0)
        .map((s, i) => ({
          start: s.offset,
          end: s.offset + s.fileSize,
          color: PALETTE[i % PALETTE.length],
          label: `Секция ${s.name} · VA 0x${hexN(s.virtualAddress)}`,
        })),
    ];
    o.set(tab.id, "exe", regions);
    return () => useOverlays.getState().clear(tab.id, "exe");
  }, [info, tab.id]);
  return { info, findings, error };
}

function CodeSection({ tab, info }: { tab: Tab; info: ExecutableInfo | null }) {
  const [arches, setArches] = useState<ArchInfo[]>([]);
  const [arch, setArch] = useState<string>("");
  const [list, setList] = useState<Instruction[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [source, setSource] = useState("");
  const [plan, setPlan] = useState<InstructionPatch | null>(null);
  useEffect(() => {
    re.arches()
      .then(setArches)
      .catch(() => {});
  }, []);
  useEffect(() => {
    if (!arch && info?.arch) setArch(info.arch);
  }, [info, arch]);
  const cur = arch || info?.arch || "x86_64";
  const start = tab.cursor;
  const address = offsetToVa(info, start) ?? start;
  useEffect(() => {
    const t = setTimeout(() => {
      re.disasm(tab.id, start, 4096, address, cur, 200)
        .then((l) => {
          setList(l);
          setErr(null);
        })
        .catch((e) => setErr(e?.message ?? String(e)));
    }, 60);
    return () => clearTimeout(t);
  }, [tab.id, tab.info.version, start, address, cur]);
  useEffect(() => {
    setPlan(null);
  }, [start]);
  const offOf = (ins: Instruction) => start + (ins.address - address);
  const first = list[0];
  const preview = async () => {
    try {
      setPlan(await re.patchPlan(tab.id, start, address, cur, source));
    } catch (e) {
      A.report(e);
    }
  };
  const apply = async () => {
    if (!plan) return;
    try {
      const r = await api.edit(tab.id, { op: "overwrite", pos: start, bytes: plan.replacement });
      A.applyResult(tab.id, r);
      setPlan(null);
      setSource("");
      toast("Инструкция заменена");
    } catch (e) {
      A.report(e);
    }
  };
  const canAsm = arches.find((a) => a.arch === cur)?.canAssemble ?? false;
  return (
    <>
      <div className="anl-cap">
        <select
          className="sel-input"
          aria-label="Архитектура"
          value={cur}
          onChange={(e) => setArch(e.target.value)}
        >
          {arches.map((a) => (
            <option key={a.arch} value={a.arch}>
              {a.label}
            </option>
          ))}
        </select>
        <span className="note mono">
          {offsetToVa(info, start) !== null ? `VA 0x${hexN(address)}` : `0x${hexN(start)}`}
        </span>
      </div>
      {err ? (
        <div className="xf-err">{err}</div>
      ) : (
        <div className="dis">
          {list.map((ins) => {
            const off = offOf(ins);
            const target =
              ins.branchTarget !== null
                ? (vaToOffset(info, ins.branchTarget) ?? (info ? null : ins.branchTarget))
                : null;
            return (
              <div
                key={ins.address}
                className={`dis-row${off === tab.cursor ? " cur" : ""}${ins.isData ? " data" : ""}`}
                onClick={() => A.selectRange(off, off + ins.size)}
                onKeyDown={(e) => e.key === "Enter" && A.selectRange(off, off + ins.size)}
                role="button"
                tabIndex={-1}
              >
                <span className="mono a">{hexN(ins.address, 8)}</span>
                <span className="mono b">
                  {ins.bytes
                    .slice(0, 6)
                    .map((b) => hexN(b, 2))
                    .join(" ")}
                </span>
                <span className="m">{ins.mnemonic}</span>
                <span className="o">
                  {target !== null ? (
                    <button
                      type="button"
                      className="link"
                      onClick={(e) => {
                        e.stopPropagation();
                        jump(target);
                      }}
                    >
                      {ins.operands}
                    </button>
                  ) : (
                    ins.operands
                  )}
                </span>
              </div>
            );
          })}
        </div>
      )}
      {canAsm && first && (
        <div className="dis-edit">
          <div className="note">
            Заменить{" "}
            <b className="mono">
              {first.mnemonic} {first.operands}
            </b>
          </div>
          <div style={{ display: "flex", gap: 6 }}>
            <input
              className="inp mono"
              placeholder="например: jmp 0x401000 ; nop"
              value={source}
              onChange={(e) => {
                setSource(e.target.value);
                setPlan(null);
              }}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") void (plan ? apply() : preview());
              }}
            />
            <button
              type="button"
              className="btn"
              onClick={plan ? apply : preview}
              disabled={!source.trim()}
            >
              {plan ? "Применить" : "Собрать"}
            </button>
          </div>
          {plan && (
            <div className="note mono" style={{ marginTop: 4 }}>
              {plan.replacement.map((b) => hexN(b, 2)).join(" ")}
              {plan.padding > 0 && ` · NOP×${plan.padding}`}
              {plan.warning && <div className="xf-err">{plan.warning}</div>}
            </div>
          )}
        </div>
      )}
    </>
  );
}

function SymList({
  title,
  items,
}: {
  title: string;
  items: {
    name: string;
    demangled: string | null;
    offset: number | null;
    address: number | null;
    library?: string | null;
  }[];
}) {
  const [q, setQ] = useState("");
  const [open, setOpen] = useState(false);
  const shown = useMemo(() => {
    const f = q.toLowerCase();
    return items.filter(
      (s) =>
        !f ||
        (s.demangled ?? s.name).toLowerCase().includes(f) ||
        s.library?.toLowerCase().includes(f),
    );
  }, [items, q]);
  if (!items.length) return null;
  return (
    <>
      <button type="button" className="anl-cap fold-h" onClick={() => setOpen(!open)}>
        <span className="cap">
          {open ? "▾" : "▸"} {title}
        </span>
        <span className="tag">{items.length}</span>
      </button>
      {open && (
        <>
          <div style={{ padding: "0 6px 4px" }}>
            <input
              className="inp"
              placeholder="Фильтр"
              value={q}
              onChange={(e) => setQ(e.target.value)}
              onKeyDown={(e) => e.stopPropagation()}
            />
          </div>
          <div className="list">
            {shown.slice(0, 500).map((s, i) => (
              <button
                type="button"
                // biome-ignore lint/suspicious/noArrayIndexKey: symbols may repeat
                key={i}
                className="str-row"
                style={{ gridTemplateColumns: "84px minmax(0,1fr)" }}
                disabled={s.offset === null}
                onClick={() => s.offset !== null && jump(s.offset)}
                title={s.demangled ?? s.name}
              >
                <span className="mono">{s.address !== null ? hexN(s.address, 8) : "—"}</span>
                <span className="tx">
                  {s.library ? `${s.library}!` : ""}
                  {s.demangled ?? s.name}
                </span>
              </button>
            ))}
          </div>
        </>
      )}
    </>
  );
}

function FileSection({
  tab,
  info,
  findings,
  error,
}: {
  tab: Tab;
  info: ExecutableInfo | null;
  findings: Finding[];
  error: string | null;
}) {
  if (!info)
    return (
      <div className="empty">
        <span>{error ? `Не исполняемый файл: ${error}` : "Читаю заголовки…"}</span>
      </div>
    );
  const fix = async () => {
    try {
      A.applyResult(tab.id, await re.fixChecksum(tab.id));
      toast("Контрольная сумма PE пересчитана");
    } catch (e) {
      A.report(e);
    }
  };
  return (
    <>
      <div className="kv anl-kv" style={{ padding: "0 6px" }}>
        <span>Формат</span>
        <b>
          {info.format.toUpperCase()} · {info.bits} бит · {info.endian}
        </b>
        <span>Машина</span>
        <b>{info.machine}</b>
        <span>Тип</span>
        <b>{info.kind}</b>
        {info.entryPoint !== null && (
          <>
            <span>Точка входа</span>
            <b>
              {info.entryOffset !== null ? (
                <button
                  type="button"
                  className="link mono"
                  onClick={() => jump(info.entryOffset ?? 0)}
                >
                  0x{hexN(info.entryPoint)}
                </button>
              ) : (
                <span className="mono">0x{hexN(info.entryPoint)}</span>
              )}
            </b>
          </>
        )}
        {info.interpreter && (
          <>
            <span>Загрузчик</span>
            <b>{info.interpreter}</b>
          </>
        )}
        {info.checksum && (
          <>
            <span>Контрольная сумма</span>
            <b>
              <span className="mono">0x{hexN(info.checksum.value, 8)}</span>{" "}
              <button type="button" className="chip" onClick={fix}>
                Пересчитать
              </button>
            </b>
          </>
        )}
      </div>
      {findings.length > 0 && (
        <>
          <div className="anl-cap">
            <span className="cap">Компилятор и упаковщики</span>
          </div>
          <div className="list">
            {findings.map((f) => (
              <button
                type="button"
                key={f.name}
                className="li"
                style={{ gridTemplateColumns: "minmax(0,1fr)", animation: "none" }}
                onClick={() =>
                  f.evidence[0] &&
                  A.selectRange(f.evidence[0].offset, f.evidence[0].offset + f.evidence[0].length)
                }
              >
                <span className="t">
                  <b>
                    {f.name}
                    {f.version ? ` ${f.version}` : ""}
                  </b>
                  <span>
                    {f.category} · {f.confidence}
                    {f.evidence[0] ? ` · ${f.evidence[0].description}` : ""}
                  </span>
                </span>
              </button>
            ))}
          </div>
        </>
      )}
      <div className="anl-cap">
        <span className="cap">Секции</span>
        <span className="tag">{info.sections.length}</span>
      </div>
      <div className="list">
        {info.sections.map((s, i) => (
          <button
            type="button"
            // biome-ignore lint/suspicious/noArrayIndexKey: section names repeat
            key={i}
            className="li"
            style={{ gridTemplateColumns: "10px minmax(0,1fr)", animation: "none" }}
            disabled={s.fileSize === 0}
            onClick={() => A.selectRange(s.offset, s.offset + s.fileSize)}
          >
            <span className="dotc" style={{ background: PALETTE[i % PALETTE.length] }} />
            <span className="t">
              <b>
                {s.name || "(без имени)"} <span className="note">{s.permissions}</span>
              </b>
              <span className="mono">
                VA {hexN(s.virtualAddress, 8)} · файл {hexN(s.offset, 8)} · {fmtSize(s.fileSize)}
              </span>
            </span>
          </button>
        ))}
      </div>
      <SymList title="Импорт" items={info.imports} />
      <SymList title="Экспорт" items={info.exports.map((e) => ({ ...e, address: e.address }))} />
      <SymList title="Символы" items={info.symbols} />
      {info.libraries.length > 0 && (
        <div className="note" style={{ padding: "6px" }}>
          Библиотеки: {info.libraries.join(", ")}
        </div>
      )}
      {info.warnings.map((w) => (
        <div key={w} className="note" style={{ padding: "0 6px" }}>
          ⚠ {w}
        </div>
      ))}
    </>
  );
}

function XrefSection({ tab, info }: { tab: Tab; info: ExecutableInfo | null }) {
  const [target, setTarget] = useState("");
  const [rep, setRep] = useState<XrefReport | null>(null);
  const [busy, setBusy] = useState(false);
  const va = offsetToVa(info, tab.cursor);
  const value = target ? Number.parseInt(target.replace(/^0x/i, ""), 16) : va;
  const run = () => {
    if (value === null || Number.isNaN(value)) return;
    setBusy(true);
    re.xrefs(tab.id, value)
      .then(setRep)
      .catch(A.report)
      .finally(() => setBusy(false));
  };
  if (!info)
    return (
      <div className="empty">
        <span>Ссылки ищутся в исполняемых файлах (PE, ELF, Mach-O).</span>
      </div>
    );
  return (
    <>
      <div className="anl-cap">
        <input
          className="inp mono"
          placeholder={va !== null ? `VA курсора 0x${hexN(va)}` : "VA (hex)"}
          value={target}
          onChange={(e) => setTarget(e.target.value)}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") run();
          }}
        />
        <button type="button" className="btn" onClick={run} disabled={busy || value === null}>
          {busy ? "Ищу…" : "Кто ссылается"}
        </button>
      </div>
      {rep && (
        <div className="list">
          {rep.code.length + rep.pointers.length === 0 && (
            <div className="note">Ссылок не найдено.</div>
          )}
          {rep.code.map((x) => (
            <button
              type="button"
              key={`c${x.from}`}
              className="str-row"
              style={{ gridTemplateColumns: "84px 44px minmax(0,1fr)" }}
              disabled={x.fromOffset === null}
              onClick={() => x.fromOffset !== null && jump(x.fromOffset)}
            >
              <span className="mono">{hexN(x.from, 8)}</span>
              <span className="enc">{x.kind}</span>
              <span className="tx">{x.text}</span>
            </button>
          ))}
          {rep.pointers.map((p) => (
            <button
              type="button"
              key={`p${p.location.offset}`}
              className="str-row"
              style={{ gridTemplateColumns: "84px 44px minmax(0,1fr)" }}
              onClick={() => A.selectRange(p.location.offset, p.location.offset + p.location.width)}
            >
              <span className="mono">{hexN(p.location.offset, 8)}</span>
              <span className="enc">{p.isRva ? "RVA" : `ptr${p.location.width * 8}`}</span>
              <span className="tx">{p.section ?? ""}</span>
            </button>
          ))}
        </div>
      )}
    </>
  );
}

function PatchSection({ tab }: { tab: Tab }) {
  const [list, setList] = useState<PatchList | null>(null);
  const [states, setStates] = useState<string[]>([]);
  const tabs = useTabs((s) => s.tabs);
  useEffect(() => {
    if (list)
      re.patchesCheck(tab.id, list)
        .then(setStates)
        .catch(() => setStates([]));
  }, [list, tab.id, tab.info.version]);
  const fromEdits = async () => {
    try {
      setList(await re.patchesExport(tab.id));
    } catch (e) {
      A.report(e);
    }
  };
  const save = async () => {
    if (!list) return;
    const path = await saveFile({
      defaultPath: `${tab.info.name}.patch.json`,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (path)
      await api
        .writeTextFile(path, JSON.stringify(list, null, 2))
        .then(() => toast("Патч сохранён"))
        .catch(A.report);
  };
  const load = async () => {
    const path = await openFile({
      multiple: false,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (typeof path !== "string") return;
    try {
      setList(JSON.parse(await api.readTextFile(path)) as PatchList);
    } catch (e) {
      A.report(e);
    }
  };
  const apply = async (revert: boolean) => {
    if (!list) return;
    try {
      A.applyResult(tab.id, await re.patchesApply(tab.id, list, revert));
      toast(revert ? "Патчи откачены" : "Патчи применены");
    } catch (e) {
      A.report(e);
    }
  };
  const port = async (to: number) => {
    if (!list) return;
    try {
      const r = await re.patchesPort(tab.id, to, list);
      const ok = r.results.filter(
        (x) => x.status === "ported" || x.status === "alreadyApplied",
      ).length;
      toast(`Перенесено ${ok} из ${r.results.length}`);
      useTabs.getState().activate(to);
      setList(r.ported);
    } catch (e) {
      A.report(e);
    }
  };
  const STATE: Record<string, string> = {
    original: "исходные",
    applied: "применён",
    mismatch: "не совпадает",
    outOfRange: "вне файла",
  };
  return (
    <>
      <div className="xf-actions">
        <button type="button" className="chip" onClick={fromEdits}>
          Из моих правок
        </button>
        <button type="button" className="chip" onClick={load}>
          Загрузить…
        </button>
        <button type="button" className="chip" onClick={save} disabled={!list}>
          Сохранить…
        </button>
      </div>
      {!list ? (
        <div className="empty">
          <span>
            Список патчей: смещение, исходные и новые байты. Его можно сохранить, применить к другой
            копии или перенести на новую версию файла.
          </span>
        </div>
      ) : (
        <>
          <div className="list">
            {list.patches.map((p, i) => (
              <button
                type="button"
                key={p.offset}
                className="str-row"
                style={{ gridTemplateColumns: "84px 90px minmax(0,1fr)" }}
                onClick={() => A.selectRange(p.offset, p.offset + p.replacement.length / 2)}
              >
                <span className="mono">{hexN(p.offset, 8)}</span>
                <span className="enc">{STATE[states[i]] ?? ""}</span>
                <span className="tx">
                  {p.original.slice(0, 16)} → {p.replacement.slice(0, 16)}
                </span>
              </button>
            ))}
          </div>
          <div className="xf-actions">
            <button type="button" className="btn pri" onClick={() => apply(false)}>
              Применить
            </button>
            <button type="button" className="btn" onClick={() => apply(true)}>
              Откатить
            </button>
            {tabs.length > 1 && (
              <select
                className="sel-input"
                aria-label="Перенести на вкладку"
                value=""
                onChange={(e) => e.target.value && port(Number(e.target.value))}
              >
                <option value="">Перенести на…</option>
                {tabs
                  .filter((t) => t.id !== tab.id)
                  .map((t) => (
                    <option key={t.id} value={t.id}>
                      {t.info.name}
                    </option>
                  ))}
              </select>
            )}
          </div>
        </>
      )}
    </>
  );
}

function ToolsSection({ tab }: { tab: Tab }) {
  const [tools, setTools] = useState<ExternalTool[]>([]);
  const [dir, setDir] = useState(() => {
    try {
      return localStorage.getItem("hx-tool-dir") ?? "";
    } catch {
      return "";
    }
  });
  useEffect(() => {
    re.tools()
      .then(setTools)
      .catch(() => {});
  }, []);
  const run = async (t: ExternalTool) => {
    const r = hasSelection(tab) ? selection(tab) : { start: 0, end: tab.info.len };
    try {
      try {
        localStorage.setItem("hx-tool-dir", dir);
      } catch {}
      const inv = await re.toolPrepare(tab.id, r.start, r.end, t.id, dir);
      if (inv.launch === "url") {
        const { openUrl } = await import("@tauri-apps/plugin-opener");
        await openUrl(inv.program);
      } else {
        await re.toolSpawn(inv.program, inv.args);
        toast(`Запущено: ${t.name}`);
      }
    } catch (e) {
      A.report(e);
    }
  };
  return (
    <>
      <div style={{ padding: "0 6px 6px" }}>
        <input
          className="inp mono"
          placeholder="Папка установки инструмента (для Ghidra, IDA…)"
          value={dir}
          onChange={(e) => setDir(e.target.value)}
          onKeyDown={(e) => e.stopPropagation()}
        />
      </div>
      <div className="list">
        {tools.map((t) => (
          <button
            type="button"
            key={t.id}
            className="li"
            style={{ gridTemplateColumns: "minmax(0,1fr)", animation: "none" }}
            onClick={() => run(t)}
          >
            <span className="t">
              <b>{t.name}</b>
              <span>{t.note}</span>
            </span>
          </button>
        ))}
      </div>
      <div className="note" style={{ padding: 6 }}>
        {hasSelection(tab) ? "Выделение" : "Файл"} сохраняется во временный файл и открывается в
        выбранной программе.
      </div>
    </>
  );
}

const SECTIONS: { id: Section; title: string }[] = [
  { id: "code", title: "Код" },
  { id: "file", title: "Файл" },
  { id: "xrefs", title: "Ссылки" },
  { id: "patches", title: "Патчи" },
  { id: "tools", title: "Внешние" },
];

export function DisasmPanel({ tab }: { tab: Tab }) {
  const [section, setSection] = useState<Section>("code");
  const { info, findings, error } = useExe(tab);
  return (
    <>
      <div className="ph">
        <span className="cap">Дизассемблер</span>
        {info && <span className="tag">{info.format.toUpperCase()}</span>}
      </div>
      <div className="seg anl-tabs" role="tablist">
        {SECTIONS.map((s) => (
          <button
            type="button"
            role="tab"
            aria-selected={section === s.id}
            key={s.id}
            className={section === s.id ? "on" : ""}
            onClick={() => setSection(s.id)}
          >
            {s.title}
          </button>
        ))}
      </div>
      {section === "code" && <CodeSection tab={tab} info={info} />}
      {section === "file" && (
        <FileSection tab={tab} info={info} findings={findings} error={error} />
      )}
      {section === "xrefs" && <XrefSection tab={tab} info={info} />}
      {section === "patches" && <PatchSection tab={tab} />}
      {section === "tools" && <ToolsSection tab={tab} />}
    </>
  );
}
