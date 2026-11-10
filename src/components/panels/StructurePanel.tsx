// Structure templates: tree synced with the grid, value editing, filter and
// export, a template editor with live diagnostics, valid-offset scan, type
// suggestions, parser generation and field-level diff (STRC-01…23,
// SRCH-11, DIFF-10).
import { open as openFile, save as saveFile } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../../ipc/api";
import { type Diagnostic, type FieldChange, type TplNode, tpl } from "../../ipc/template";
import { fmtSize, hexN } from "../../lib/format";
import * as A from "../../state/actions";
import { useSettings } from "../../state/settings";
import { hasSelection, selection, type Tab, useTabs } from "../../state/tabs";
import {
  applyTemplate,
  autoApply,
  reapplyAll,
  removeTemplate,
  type TabTemplate,
  useTpl,
} from "../../state/templates";
import { toast } from "../../state/ui";
import "../../styles/structure.css";
import { Icon } from "../icons";

const PAGE = 500;
/** Stable empty list for store selectors. */
const NONE: TabTemplate[] = [];
const stop = (e: React.KeyboardEvent) => e.stopPropagation();

type Section = "tree" | "editor" | "tools";

interface Row {
  rk: string;
  key: number;
  node: TplNode;
  depth: number;
  expandable: boolean;
  more?: { parent: string; key: number; node: TplNode; from: number };
}

const rowKey = (key: number, n: TplNode) => `${key}:${n.id}:${n.elemIndex ?? ""}`;
const expandable = (n: TplNode) =>
  n.childCount > 0 || (n.lazy && (n.kind === "struct" || n.kind === "union"));

function nodeColor(n: TplNode): string | undefined {
  if (!n.color) return undefined;
  if ("rgb" in n.color) return `#${n.color.rgb.replace(/^#/, "")}`;
  return ["#2F6BF0", "#1F9D74", "#E8890C", "#7C4DFF", "#D6336C", "#0EA5E9"][n.color.palette % 6];
}

function Tree({ tab }: { tab: Tab }) {
  const list = useTpl((s) => s.byTab[tab.id] ?? NONE);
  const currentKey = useTpl((s) => s.current[tab.id]);
  const cur = list.find((t) => t.key === currentKey) ?? list[0];
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [kids, setKids] = useState<Map<string, { key: number; nodes: TplNode[] }>>(new Map());
  const [hit, setHit] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [visible, setVisible] = useState<Set<number> | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // A new evaluation invalidates loaded children.
  useEffect(() => {
    setKids(new Map());
  }, [cur]);

  const load = useCallback(async (key: number, n: TplNode, from = 0) => {
    const rk = rowKey(key, n);
    try {
      if (n.lazy && n.elemIndex !== null && !n.isArray) {
        const r = await tpl.expand(key, n.id, n.elemIndex);
        setKids((m) => new Map(m).set(rk, r));
        return;
      }
      const nodes = await tpl.children(key, n.id, from, from + PAGE);
      setKids((m) => {
        const prev = from ? (m.get(rk)?.nodes ?? []) : [];
        return new Map(m).set(rk, { key, nodes: [...prev, ...nodes] });
      });
    } catch (e) {
      A.report(e);
    }
  }, []);

  const toggle = (r: Row) => {
    const next = new Set(open);
    if (next.has(r.rk)) next.delete(r.rk);
    else {
      next.add(r.rk);
      if (!kids.has(r.rk)) void load(r.key, r.node);
    }
    setOpen(next);
  };

  // Grid → tree: reveal the field under the cursor (STRC-13).
  useEffect(() => {
    if (!cur) return;
    const t = setTimeout(async () => {
      const h = await tpl.nodeAt(cur.key, tab.cursor).catch(() => null);
      if (!h?.path.length) return setHit(null);
      const next = new Set(open);
      let changed = false;
      for (const id of h.path.slice(0, -1)) {
        const rk = `${cur.key}:${id}:`;
        if (!next.has(rk)) {
          next.add(rk);
          changed = true;
          if (!kids.has(rk)) {
            const nodes = await tpl.children(cur.key, id, 0, PAGE).catch(() => []);
            setKids((m) => new Map(m).set(rk, { key: cur.key, nodes }));
          }
        }
      }
      if (changed) setOpen(next);
      const last = h.path[h.path.length - 1];
      setHit(`${cur.key}:${last}:${h.elementIndex ?? ""}`);
    }, 120);
    return () => clearTimeout(t);
  }, [cur, tab.cursor]);

  useEffect(() => {
    if (!hit) return;
    listRef.current
      ?.querySelector(`[data-rk="${CSS.escape(hit)}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [hit]);

  // Filter (STRC-15).
  useEffect(() => {
    if (!cur || !filter.trim()) return setVisible(null);
    const t = setTimeout(() => {
      tpl
        .filter(cur.key, filter)
        .then(async (ids) => {
          const set = new Set(ids);
          setVisible(set);
          // Open every matching ancestor so hits are visible.
          const loaded = new Map(kids);
          const opened = new Set<string>();
          for (const id of ids) {
            const rk = `${cur.key}:${id}:`;
            opened.add(rk);
            if (!loaded.has(rk))
              loaded.set(rk, {
                key: cur.key,
                nodes: await tpl.children(cur.key, id, 0, PAGE).catch(() => []),
              });
          }
          setKids(loaded);
          setOpen(opened);
        })
        .catch(A.report);
    }, 250);
    return () => clearTimeout(t);
  }, [cur, filter]);

  const rows = useMemo(() => {
    const out: Row[] = [];
    if (!cur) return out;
    const walk = (
      key: number,
      nodes: TplNode[],
      depth: number,
      parent: string | null,
      parentNode: TplNode | null,
    ) => {
      for (const n of nodes) {
        if (n.hidden) continue;
        if (visible && n.elemIndex === null && !visible.has(n.id)) continue;
        const rk = rowKey(key, n);
        out.push({ rk, key, node: n, depth, expandable: expandable(n) });
        if (open.has(rk)) {
          const c = kids.get(rk);
          if (c) walk(c.key, c.nodes, depth + 1, rk, n);
        }
      }
      if (parent && parentNode) {
        const loaded = kids.get(parent)?.nodes.length ?? 0;
        if (parentNode.childCount > loaded && !(parentNode.lazy && parentNode.elemIndex !== null))
          out.push({
            rk: `${parent}:more`,
            key,
            node: parentNode,
            depth,
            expandable: false,
            more: { parent, key, node: parentNode, from: loaded },
          });
      }
    };
    walk(cur.key, cur.roots, 0, null, null);
    return out.slice(0, 20_000);
  }, [cur, open, kids, visible]);

  const commit = async (r: Row, text: string) => {
    setEditing(null);
    try {
      const res = await tpl.setValue(r.key, r.node.id, r.node.elemIndex, text);
      A.applyResult(tab.id, res);
    } catch (e) {
      A.report(e);
    }
  };

  const exportTree = async () => {
    if (!cur) return;
    const path = await saveFile({
      defaultPath: `${tab.info.name}.${cur.name}.json`,
      filters: [
        { name: "JSON", extensions: ["json"] },
        { name: "CSV", extensions: ["csv"] },
        { name: "XML", extensions: ["xml"] },
      ],
    });
    if (!path) return;
    const ext = path.split(".").pop()?.toLowerCase();
    await tpl
      .export(cur.key, ext === "csv" ? "csv" : ext === "xml" ? "xml" : "json", path)
      .then(() => toast("Дерево сохранено"))
      .catch(A.report);
  };

  if (!cur)
    return (
      <div className="empty">
        <span>
          Шаблон не применён. Выберите формат из библиотеки, откройте файл шаблона или напишите свой
          во вкладке «Редактор».
        </span>
      </div>
    );
  return (
    <>
      <div style={{ display: "flex", gap: 6, padding: "0 4px 6px" }}>
        <input
          className="inp"
          placeholder="Фильтр: имя, тип, значение"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={stop}
          aria-label="Фильтр полей"
        />
        <button type="button" className="chip" onClick={exportTree}>
          Экспорт
        </button>
      </div>
      {cur.errors.length > 0 && (
        <div className="st-errors">
          {cur.errors.slice(0, 5).map((e) => (
            <button
              type="button"
              key={`${e.line}:${e.col}:${e.message}`}
              className={e.warning ? "warn" : "err"}
              onClick={() => A.setCursor(e.offset, false, { jump: true })}
            >
              {e.line ? `${e.line}:${e.col} ` : ""}
              {e.message}
            </button>
          ))}
        </div>
      )}
      <div className="st-tree" ref={listRef} role="tree" aria-label="Дерево структуры">
        {rows.map((r) =>
          r.more ? (
            <button
              type="button"
              key={r.rk}
              className="st-row more"
              style={{ paddingLeft: 8 + r.depth * 14 }}
              onClick={() => r.more && load(r.more.key, r.more.node, r.more.from)}
            >
              ещё {Math.min(PAGE, r.node.childCount - r.more.from).toLocaleString("ru")} из{" "}
              {r.node.childCount.toLocaleString("ru")}…
            </button>
          ) : (
            <div
              key={r.rk}
              data-rk={r.rk}
              role="treeitem"
              aria-expanded={r.expandable ? open.has(r.rk) : undefined}
              aria-selected={hit === r.rk}
              tabIndex={-1}
              className={`st-row${hit === r.rk ? " hit" : ""}${r.node.error ? " bad" : ""}`}
              style={{ paddingLeft: 4 + r.depth * 14 }}
              onClick={() => A.selectRange(r.node.offset, r.node.offset + Math.max(1, r.node.size))}
              onKeyDown={(e) =>
                e.key === "Enter" &&
                A.selectRange(r.node.offset, r.node.offset + Math.max(1, r.node.size))
              }
              title={
                r.node.comment ??
                r.node.error ??
                `${r.node.typeName} · 0x${hexN(r.node.offset)} · ${fmtSize(r.node.size)}`
              }
            >
              <button
                type="button"
                className="tw"
                aria-label={open.has(r.rk) ? "Свернуть" : "Развернуть"}
                style={{ visibility: r.expandable ? "visible" : "hidden" }}
                onClick={(e) => {
                  e.stopPropagation();
                  toggle(r);
                }}
              >
                {open.has(r.rk) ? "▾" : "▸"}
              </button>
              <span className="sw" style={{ background: nodeColor(r.node) ?? "transparent" }} />
              <span className="nm">
                {r.node.elemIndex !== null
                  ? `[${r.node.elemIndex}]`
                  : (r.node.displayName ?? r.node.name)}
              </span>
              <span className="ty">
                {r.node.isArray ? `${r.node.typeName}[${r.node.len}]` : r.node.typeName}
              </span>
              {editing === r.rk ? (
                <input
                  className="inp mono st-edit"
                  defaultValue={r.node.value ?? ""}
                  // biome-ignore lint/a11y/noAutofocus: inline edit starts on double click
                  autoFocus
                  aria-label="Новое значение"
                  onClick={(e) => e.stopPropagation()}
                  onBlur={() => setEditing(null)}
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key === "Enter") void commit(r, e.currentTarget.value);
                    if (e.key === "Escape") setEditing(null);
                  }}
                />
              ) : (
                <span
                  className="vl mono"
                  onDoubleClick={(e) => {
                    e.stopPropagation();
                    if (r.node.value !== null && !r.node.children.length) setEditing(r.rk);
                  }}
                >
                  {r.node.enumName ?? r.node.value ?? ""}
                </span>
              )}
              <span className="of mono">{hexN(r.node.offset)}</span>
            </div>
          ),
        )}
      </div>
      <div className="note" style={{ padding: "4px 6px" }}>
        {cur.nodeCount.toLocaleString("ru")} узлов{cur.truncated ? " · достигнут лимит узлов" : ""}{" "}
        · двойной клик по значению — изменить
      </div>
      {cur.log.length > 0 && <pre className="st-log">{cur.log.join("\n")}</pre>}
    </>
  );
}

// i18n-ignore
const SAMPLE = `// Своя структура: поля читаются подряд от курсора.
struct Header {
  char magic[4];
  u32 version;
  u32 count;
  u64 offset [[comment("смещение таблицы")]];
};

Header header @ 0;
`;

function Editor({ tab }: { tab: Tab }) {
  const [text, setText] = useState(() => {
    try {
      return localStorage.getItem("hx-template") ?? SAMPLE;
    } catch {
      return SAMPLE;
    }
  });
  const [dialect, setDialect] = useState("hexpat");
  const [diags, setDiags] = useState<Diagnostic[]>([]);
  const [lib, setLib] = useState<{ id: string; name: string }[]>([]);
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    tpl
      .library()
      .then(setLib)
      .catch(() => {});
  }, []);
  useEffect(() => {
    const t = setTimeout(() => {
      try {
        localStorage.setItem("hx-template", text);
      } catch {}
      tpl
        .diagnostics(text, dialect)
        .then(setDiags)
        .catch(() => setDiags([]));
    }, 250);
    return () => clearTimeout(t);
  }, [text, dialect]);
  const jump = (d: Diagnostic) => {
    const ta = ref.current;
    if (!ta) return;
    const lines = text.split("\n");
    const pos =
      lines.slice(0, d.line - 1).reduce((n, l) => n + l.length + 1, 0) + Math.max(0, d.col - 1);
    ta.focus();
    ta.setSelectionRange(pos, pos);
  };
  const sel = hasSelection(tab) ? selection(tab) : null;
  const run = () =>
    applyTemplate(
      tab.id,
      { kind: "text", text, dialect },
      sel ? { offset: sel.start, start: sel.start, end: sel.end } : {},
    );
  const openFromFile = async () => {
    const p = await openFile({
      multiple: false,
      filters: [{ name: "Шаблоны", extensions: ["hexpat", "hexa", "bt", "ksy", "h", "c"] }],
    });
    if (typeof p !== "string") return;
    setText(await api.readTextFile(p));
    const ext = p.split(".").pop()?.toLowerCase() ?? "";
    setDialect(
      ext === "bt" ? "bt" : ext === "ksy" ? "ksy" : ext === "h" || ext === "c" ? "c" : "hexpat",
    );
  };
  const saveToFile = async () => {
    const ext = dialect === "c" ? "h" : dialect;
    const p = await saveFile({
      defaultPath: `template.${ext}`,
      filters: [{ name: "Шаблон", extensions: [ext] }],
    });
    if (p)
      await api
        .writeTextFile(p, text)
        .then(() => toast("Шаблон сохранён"))
        .catch(A.report);
  };
  const errors = diags.filter((d) => d.severity === "error").length;
  return (
    <>
      <div className="xf-actions" style={{ paddingTop: 0 }}>
        <select
          className="sel-input"
          aria-label="Язык шаблона"
          value={dialect}
          onChange={(e) => setDialect(e.target.value)}
        >
          <option value="hexpat">Hexa / ImHex</option>
          <option value="bt">010 Editor .bt</option>
          <option value="c">C-заголовок</option>
          <option value="ksy">Kaitai .ksy</option>
        </select>
        <select
          className="sel-input"
          aria-label="Взять из библиотеки"
          value=""
          onChange={async (e) => {
            if (!e.target.value) return;
            setText(await tpl.librarySource(e.target.value));
            setDialect("hexpat");
          }}
        >
          <option value="">из библиотеки…</option>
          {lib.map((l) => (
            <option key={l.id} value={l.id}>
              {l.name}
            </option>
          ))}
        </select>
      </div>
      <textarea
        ref={ref}
        className="inp mono st-src"
        spellCheck={false}
        aria-label="Исходный текст шаблона"
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) void run();
          if (e.key === "Tab") {
            e.preventDefault();
            const t = e.currentTarget;
            const s = t.selectionStart;
            setText(`${text.slice(0, s)}  ${text.slice(t.selectionEnd)}`);
            requestAnimationFrame(() => t.setSelectionRange(s + 2, s + 2));
          }
        }}
      />
      <div className="st-diags">
        {diags.length === 0 ? (
          <span className="note">Ошибок нет</span>
        ) : (
          diags.slice(0, 20).map((d) => (
            <button
              type="button"
              key={`${d.line}:${d.col}:${d.message}`}
              className={d.severity === "error" ? "err" : "warn"}
              onClick={() => jump(d)}
            >
              {d.line}:{d.col} {d.message}
            </button>
          ))
        )}
      </div>
      <div className="xf-actions">
        <button type="button" className="btn pri" onClick={run} disabled={errors > 0}>
          Применить {sel ? "к выделению" : ""} <span className="kbd">⌘↵</span>
        </button>
        <button type="button" className="chip" onClick={openFromFile}>
          Открыть…
        </button>
        <button type="button" className="chip" onClick={saveToFile}>
          Сохранить…
        </button>
      </div>
    </>
  );
}

const TARGETS: [string, string, string][] = [
  ["ksy", "Kaitai .ksy", "ksy"],
  ["c", "C-заголовок", "h"],
  ["python", "Python", "py"],
  ["go", "Go", "go"],
  ["typescript", "TypeScript", "ts"],
  ["rust", "Rust", "rs"],
];

function Tools({ tab }: { tab: Tab }) {
  const list = useTpl((s) => s.byTab[tab.id] ?? NONE);
  const currentKey = useTpl((s) => s.current[tab.id]);
  const cur = list.find((t) => t.key === currentKey) ?? list[0];
  const tabs = useTabs((s) => s.tabs);
  const others = useTpl((s) => s.byTab);
  const [type, setType] = useState(cur?.roots[0]?.typeName ?? "");
  const [hits, setHits] = useState<number[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [sugg, setSugg] = useState<
    { ty: string; value: string; confidence: number; reason: string }[]
  >([]);
  const [changes, setChanges] = useState<FieldChange[] | null>(null);
  useEffect(() => {
    tpl
      .suggest(tab.id, tab.cursor)
      .then(setSugg)
      .catch(() => setSugg([]));
  }, [tab.id, tab.cursor, tab.info.version]);
  const scan = async () => {
    if (!cur || !type) return;
    setBusy(true);
    try {
      setHits(await tpl.scan(tab.id, cur.source, type, 0, tab.info.len, 1));
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  const generate = async (target: string, ext: string) => {
    if (!cur) return;
    try {
      const code = await tpl.generate(cur.source, target, cur.roots[0]?.typeName ?? null);
      const path = await saveFile({ defaultPath: `${cur.roots[0]?.typeName ?? "parser"}.${ext}` });
      if (path) await api.writeTextFile(path, code).then(() => toast("Сохранено"));
    } catch (e) {
      A.report(e);
    }
  };
  const compareWith = async (otherKey: number) => {
    if (!cur) return;
    try {
      setChanges(await tpl.diff(cur.key, otherKey));
    } catch (e) {
      A.report(e);
    }
  };
  const otherApplied = tabs.flatMap((t) =>
    t.id === tab.id
      ? []
      : (others[t.id] ?? []).map((a) => ({ key: a.key, label: `${t.info.name} · ${a.name}` })),
  );
  return (
    <>
      <div className="anl-cap">
        <span className="cap">Что это за байты</span>
        <span className="note mono">0x{hexN(tab.cursor)}</span>
      </div>
      <div className="list">
        {sugg.slice(0, 6).map((s) => (
          <div
            key={s.ty}
            className="str-row"
            style={{ gridTemplateColumns: "70px minmax(0,1fr) 36px" }}
            title={s.reason}
          >
            <span className="mono">{s.ty}</span>
            <span className="tx">{s.value}</span>
            <span className="enc">{Math.round(s.confidence * 100)}%</span>
          </div>
        ))}
      </div>
      {cur && (
        <>
          <div className="anl-cap">
            <span className="cap">Где тип валиден</span>
          </div>
          <div style={{ display: "flex", gap: 6, padding: "0 6px" }}>
            <input
              className="inp mono"
              value={type}
              onChange={(e) => setType(e.target.value)}
              onKeyDown={stop}
              aria-label="Имя типа"
              placeholder="Header"
            />
            <button type="button" className="btn" onClick={scan} disabled={busy || !type}>
              {busy ? "Ищу…" : "Найти"}
            </button>
          </div>
          {hits && (
            <div className="fif-offs" style={{ padding: "6px" }}>
              {hits.length === 0 && <span className="note">Нигде</span>}
              {hits.slice(0, 200).map((o) => (
                <button
                  type="button"
                  key={o}
                  className="chip mono"
                  onClick={() => {
                    A.setCursor(o, false, { jump: true });
                    void applyTemplate(tab.id, cur.source, { offset: o, rootType: type });
                  }}
                  title="Применить тип по этому смещению"
                >
                  {hexN(o)}
                </button>
              ))}
              {hits.length > 200 && <span className="note">+{hits.length - 200}</span>}
            </div>
          )}
          <div className="anl-cap">
            <span className="cap">Экспорт шаблона</span>
          </div>
          <div className="xf-actions" style={{ paddingTop: 0 }}>
            {TARGETS.map(([id, label, ext]) => (
              <button type="button" key={id} className="chip" onClick={() => generate(id, ext)}>
                {label}
              </button>
            ))}
          </div>
          {otherApplied.length > 0 && (
            <>
              <div className="anl-cap">
                <span className="cap">Сравнить поля</span>
              </div>
              <div style={{ padding: "0 6px" }}>
                <select
                  className="sel-input"
                  aria-label="С чем сравнить"
                  value=""
                  onChange={(e) => e.target.value && compareWith(Number(e.target.value))}
                >
                  <option value="">с шаблоном на вкладке…</option>
                  {otherApplied.map((o) => (
                    <option key={o.key} value={o.key}>
                      {o.label}
                    </option>
                  ))}
                </select>
              </div>
              {changes && (
                <div className="list">
                  {changes.length === 0 && <div className="note">Поля совпадают</div>}
                  {changes.slice(0, 500).map((c) => (
                    <button
                      type="button"
                      key={c.path}
                      className="str-row"
                      style={{ gridTemplateColumns: "minmax(0,1fr)" }}
                      onClick={() =>
                        c.oldOffset !== null && A.setCursor(c.oldOffset, false, { jump: true })
                      }
                    >
                      <span className="tx">
                        <b className={`k-${c.kind}`}>{c.path}</b> {c.old ?? "—"} → {c.new ?? "—"}
                      </span>
                    </button>
                  ))}
                </div>
              )}
            </>
          )}
        </>
      )}
    </>
  );
}

export function StructurePanel({ tab }: { tab: Tab }) {
  const [section, setSection] = useState<Section>("tree");
  const list = useTpl((s) => s.byTab[tab.id] ?? NONE);
  const currentKey = useTpl((s) => s.current[tab.id]);
  const auto = useSettings((s) => s.s.autoTemplate);
  const [lib, setLib] = useState<{ id: string; name: string; description: string }[]>([]);
  const [detected, setDetected] = useState<string[]>([]);
  useEffect(() => {
    tpl
      .library()
      .then(setLib)
      .catch(() => {});
  }, []);
  useEffect(() => {
    tpl
      .detect(tab.id)
      .then(setDetected)
      .catch(() => setDetected([]));
    if (auto) void autoApply(tab.id);
  }, [tab.id, auto]);
  // Live re-evaluation after edits (STRC-11).
  const version = useRef(tab.info.version);
  useEffect(() => {
    if (version.current === tab.info.version) return;
    version.current = tab.info.version;
    const t = setTimeout(() => reapplyAll(tab.id), 300);
    return () => clearTimeout(t);
  }, [tab.id, tab.info.version]);

  const apply = (id: string) => {
    const sel = hasSelection(tab) ? selection(tab) : null;
    void applyTemplate(tab.id, { kind: "library", id }, sel ? { offset: sel.start } : {});
  };
  const fromFile = async () => {
    const p = await openFile({
      multiple: false,
      filters: [{ name: "Шаблоны", extensions: ["hexpat", "hexa", "bt", "ksy", "h", "c"] }],
    });
    if (typeof p === "string") void applyTemplate(tab.id, { kind: "file", path: p });
  };
  return (
    <>
      <div className="ph">
        <span className="cap">Структура</span>
        <div className="grow" />
        <select
          className="sel-input"
          aria-label="Применить шаблон"
          value=""
          onChange={(e) => e.target.value && apply(e.target.value)}
        >
          <option value="">+ шаблон</option>
          {detected.length > 0 && (
            <optgroup label="Подходит к файлу">
              {detected.map((id) => (
                <option key={`d${id}`} value={id}>
                  {lib.find((l) => l.id === id)?.name ?? id}
                </option>
              ))}
            </optgroup>
          )}
          <optgroup label="Библиотека">
            {lib.map((l) => (
              <option key={l.id} value={l.id}>
                {l.name}
              </option>
            ))}
          </optgroup>
        </select>
        <button
          type="button"
          className="ib"
          aria-label="Шаблон из файла"
          title="Шаблон из файла (.hexpat, .bt, .ksy, .h)"
          onClick={fromFile}
        >
          <Icon.plus />
        </button>
      </div>
      {list.length > 0 && (
        <div className="xf-saved" style={{ paddingTop: 0 }}>
          {list.map((t) => (
            <span key={t.key} className={`chip st-chip${t.key === currentKey ? " on" : ""}`}>
              <button type="button" onClick={() => useTpl.getState().set(tab.id, list, t.key)}>
                {t.name}
              </button>
              <button
                type="button"
                aria-label="Убрать шаблон"
                onClick={() => removeTemplate(tab.id, t.key)}
              >
                ×
              </button>
            </span>
          ))}
        </div>
      )}
      <div className="seg anl-tabs" role="tablist">
        {(
          [
            ["tree", "Дерево"],
            ["editor", "Редактор"],
            ["tools", "Инструменты"],
          ] as const
        ).map(([id, title]) => (
          <button
            type="button"
            role="tab"
            key={id}
            aria-selected={section === id}
            className={section === id ? "on" : ""}
            onClick={() => setSection(id)}
          >
            {title}
          </button>
        ))}
      </div>
      {section === "tree" && <Tree tab={tab} />}
      {section === "editor" && <Editor tab={tab} />}
      {section === "tools" && <Tools tab={tab} />}
    </>
  );
}
