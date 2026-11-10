// Command palette, goto, context menu and toast.
import { useEffect, useMemo, useRef, useState } from "react";
import { COMMANDS, type Command, keysFor, layoutCommands, runCommand } from "../lib/commands";
import { evalExpr } from "../lib/expr";
import { groupDec, hexN } from "../lib/format";
import { displayKeys } from "../lib/keys";
import * as A from "../state/actions";
import { useSettings } from "../state/settings";
import { activeTab, selection, useActiveTab } from "../state/tabs";
import { type MenuItem, useUi } from "../state/ui";
import { Icon } from "./icons";

const EN = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";
const RU = "йцукенгшщзхъфывапролджэячсмитьбюё";

/** Text typed in the wrong keyboard layout ("ntvf" → "тема"). */
export function switchLayout(q: string): string {
  return [...q.toLowerCase()].map((c) => RU[EN.indexOf(c)] ?? EN[RU.indexOf(c)] ?? c).join("");
}

/** Subsequence fuzzy match; returns matched indices or null. */
export function fuzzy(text: string, q: string): number[] | null {
  const t = text.toLowerCase();
  const out: number[] = [];
  let j = 0;
  for (const ch of q.toLowerCase()) {
    if (ch === " ") continue;
    j = t.indexOf(ch, j);
    if (j < 0) return null;
    out.push(j++);
  }
  return out;
}

function score(idx: number[]): number {
  let s = 0;
  for (let i = 1; i < idx.length; i++)
    s += idx[i] - idx[i - 1] === 1 ? 3 : -0.2 * (idx[i] - idx[i - 1]);
  return s - (idx[0] ?? 0) * 0.5;
}

function Highlight({ text, idx }: { text: string; idx: number[] }) {
  const set = new Set(idx);
  return (
    <span>
      {[...text].map((c, i) =>
        set.has(i) ? (
          // biome-ignore lint/suspicious/noArrayIndexKey: static text
          <mark key={i}>{c}</mark>
        ) : (
          c
        ),
      )}
    </span>
  );
}

export function Palette() {
  const ui = useUi();
  const tab = useActiveTab();
  const [q, setQ] = useState(ui.paletteQuery);
  const [sel, setSel] = useState(0);
  const list = useRef<HTMLDivElement>(null);
  const close = () => ui.set({ palette: false, paletteQuery: "" });

  const items = useMemo(() => {
    const all: Command[] = [...COMMANDS, ...layoutCommands()].filter(
      (c) => !c.hidden && (!c.doc || tab),
    );
    const out: { c: Command; idx: number[]; s: number }[] = [];
    // An offset typed directly jumps there.
    if (tab && q.trim()) {
      try {
        const r = evalExpr(q, {
          cursor: tab.cursor,
          length: tab.info.len,
          bytesPerRow: A.viewport.bpr,
        });
        if (r.value >= 0 && r.value <= tab.info.len) {
          out.push({
            c: {
              id: "__goto",
              title: `Перейти к 0x${hexN(r.value)}`,
              group: "Переход",
              hint: groupDec(r.value),
              run: () => A.jumpTo(r.value),
            },
            idx: [],
            s: 1000,
          });
        }
      } catch {
        /* not an offset */
      }
    }
    for (const c of all) {
      if (!q.trim()) {
        out.push({ c, idx: [], s: 0 });
        continue;
      }
      const alt = switchLayout(q);
      const idx =
        fuzzy(c.title, q) ??
        fuzzy(`${c.group} ${c.title}`, q) ??
        fuzzy(c.title, alt) ??
        fuzzy(c.id, q);
      if (idx) out.push({ c, idx: fuzzy(c.title, q) ?? fuzzy(c.title, alt) ?? [], s: score(idx) });
    }
    if (q.trim()) out.sort((a, b) => b.s - a.s);
    return out.slice(0, 200);
  }, [q, tab]);

  useEffect(() => setSel(0), []);
  useEffect(() => {
    list.current?.querySelector(".pi.on")?.scrollIntoView({ block: "nearest" });
  }, []);

  const run = (c: Command) => {
    close();
    if (c.id === "__goto") c.run();
    else runCommand(c.id);
  };

  const onKey = (e: React.KeyboardEvent) => {
    e.stopPropagation();
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSel((s) => Math.min(items.length - 1, s + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSel((s) => Math.max(0, s - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (items[sel]) run(items[sel].c);
    } else if (e.key === "Escape") {
      close();
    }
  };

  let lastGroup = "";
  return (
    <div className="scrim" onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div className="pal" role="dialog" aria-label="Командная палитра">
        <div className="pin">
          <Icon.search />
          <input
            // biome-ignore lint/a11y/noAutofocus: palette is opened by the user
            autoFocus
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={onKey}
            placeholder="Команда или смещение, например 0x40…"
            spellCheck={false}
            autoComplete="off"
            aria-label="Команда"
          />
        </div>
        <div className="plist" ref={list}>
          {items.map((it, i) => {
            const header = !q.trim() && it.c.group !== lastGroup ? it.c.group : null;
            lastGroup = it.c.group;
            return (
              <div key={it.c.id}>
                {header && <div className="pgh">{header}</div>}
                <button
                  type="button"
                  className={`pi${i === sel ? " on" : ""}`}
                  onClick={() => run(it.c)}
                  onMouseMove={() => setSel(i)}
                >
                  {it.idx.length ? (
                    <Highlight text={it.c.title} idx={it.idx} />
                  ) : (
                    <span>{it.c.title}</span>
                  )}
                  <span className="pd">{it.c.hint ?? (q.trim() ? it.c.group : "")}</span>
                  <span className="kbd">{displayKeys(keysFor(it.c.id))}</span>
                </button>
              </div>
            );
          })}
          {items.length === 0 && (
            <div className="pempty">
              Ничего не нашлось. Попробуйте «копировать», «тема» или смещение вроде 0x21.
            </div>
          )}
        </div>
        <div className="pfoot">
          <span>
            <span className="kbd">↑</span>
            <span className="kbd">↓</span> выбрать
          </span>
          <span>
            <span className="kbd">↵</span> выполнить
          </span>
          <span>
            <span className="kbd">esc</span> закрыть
          </span>
        </div>
      </div>
    </div>
  );
}

const KIND_TEXT = {
  absolute: "абсолютное",
  relative: "от курсора",
  fromEnd: "от конца",
  percent: "процент файла",
  sector: "сектор",
  page: "страница 4 КБ",
  row: "строка",
};

export function GotoDialog() {
  const set = useUi((u) => u.set);
  const tab = useActiveTab();
  const s = useSettings((st) => st.s);
  const [q, setQ] = useState("");
  if (!tab) return null;
  const sel = selection(tab);
  let preview: { ok: true; value: number; kind: string } | { ok: false; err: string } | null = null;
  if (q.trim()) {
    try {
      const r = evalExpr(q, {
        cursor: tab.cursor,
        length: tab.info.len,
        selStart: sel.start,
        selEnd: sel.end,
        bytesPerRow: A.viewport.bpr,
        segmentSize: s.offsetSegment,
      });
      const v = r.value - (r.kind === "absolute" ? s.baseAddress : 0);
      preview =
        v < 0 || v > tab.info.len
          ? {
              ok: false,
              err: `0x${hexN(Math.abs(r.value))} за пределами файла (0…0x${hexN(tab.info.len)})`,
            }
          : { ok: true, value: v, kind: KIND_TEXT[r.kind] };
    } catch (e) {
      preview = { ok: false, err: (e as Error).message };
    }
  }
  const close = () => set({ goto: false });
  return (
    <div className="scrim" onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div className="pal goto" role="dialog" aria-label="Перейти к смещению">
        <div className="pin">
          <Icon.goto />
          <input
            className="mono"
            // biome-ignore lint/a11y/noAutofocus: dialog
            autoFocus
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Escape") close();
              if (e.key === "Enter" && preview?.ok) {
                close();
                if (e.shiftKey)
                  A.selectRange(
                    Math.min(tab.cursor, preview.value),
                    Math.max(tab.cursor, preview.value),
                  );
                else A.jumpTo(preview.value);
              }
            }}
            placeholder="0x21"
            spellCheck={false}
            autoComplete="off"
            aria-label="Смещение или выражение"
          />
        </div>
        {preview && (
          <div className={`gprev${preview.ok ? "" : " err"}`}>
            {preview.ok ? (
              <>
                <span>
                  0x{hexN(preview.value, 8)}{" "}
                  <span className="mut">· {groupDec(preview.value)}</span>
                </span>
                <span className="mut">{preview.kind} · ⇧↵ выделить</span>
              </>
            ) : (
              <span>{preview.err}</span>
            )}
          </div>
        )}
        <div className="pfoot">
          <span>Числа — hex, десятичные с суффиксом d: 128d</span>
          <span>end · cursor · sel + − ×</span>
          <span>+N — от курсора · ^N — от конца · 50% · s:N сектор · r:N строка</span>
        </div>
      </div>
    </div>
  );
}

function MenuList({
  items,
  onDone,
  x,
  y,
}: {
  items: MenuItem[];
  onDone: () => void;
  x: number;
  y: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [sub, setSub] = useState<{ i: number; x: number; y: number } | null>(null);
  const [pos, setPos] = useState({ x, y });
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      x: Math.min(x, window.innerWidth - r.width - 8),
      y: Math.min(y, window.innerHeight - r.height - 8),
    });
    el.querySelector<HTMLButtonElement>("button.mi")?.focus();
  }, [x, y]);
  return (
    <>
      <div
        ref={ref}
        className="ctx"
        style={{ left: pos.x, top: pos.y }}
        role="menu"
        onKeyDown={(e) => {
          e.stopPropagation();
          const buttons = [
            ...(ref.current?.querySelectorAll<HTMLButtonElement>("button.mi") ?? []),
          ];
          const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
          if (e.key === "ArrowDown") buttons[(i + 1) % buttons.length]?.focus();
          if (e.key === "ArrowUp") buttons[(i - 1 + buttons.length) % buttons.length]?.focus();
          if (e.key === "Escape") onDone();
        }}
      >
        {items.map((m, i) => {
          if (m.header)
            return (
              // biome-ignore lint/suspicious/noArrayIndexKey: static menu
              <div key={i} className="mh">
                {m.header}
              </div>
            );
          // biome-ignore lint/suspicious/noArrayIndexKey: static menu
          if (m.separator) return <div key={i} className="ms" />;
          return (
            <button
              type="button"
              // biome-ignore lint/suspicious/noArrayIndexKey: static menu
              key={i}
              className={`mi${sub?.i === i ? " on" : ""}`}
              role="menuitem"
              disabled={m.disabled}
              onMouseEnter={(e) => {
                if (m.submenu) {
                  const r = e.currentTarget.getBoundingClientRect();
                  setSub({ i, x: r.right + 2, y: r.top - 5 });
                } else setSub(null);
              }}
              onClick={(e) => {
                if (m.submenu) {
                  const r = e.currentTarget.getBoundingClientRect();
                  setSub({ i, x: r.right + 2, y: r.top - 5 });
                  return;
                }
                onDone();
                m.run?.();
              }}
            >
              <span>{m.label}</span>
              <span className="kbd">{m.submenu ? "›" : m.keys}</span>
            </button>
          );
        })}
      </div>
      {sub && items[sub.i]?.submenu && (
        <MenuList items={items[sub.i].submenu!} onDone={onDone} x={sub.x} y={sub.y} />
      )}
    </>
  );
}

export function ContextMenu() {
  const menu = useUi((u) => u.menu);
  const set = useUi((u) => u.set);
  useEffect(() => {
    if (!menu) return;
    const close = (e: MouseEvent) => {
      if (!(e.target as HTMLElement).closest(".ctx")) set({ menu: null });
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [menu, set]);
  if (!menu) return null;
  return <MenuList items={menu.items} x={menu.x} y={menu.y} onDone={() => set({ menu: null })} />;
}

export function ToastView() {
  const toast = useUi((u) => u.toast);
  const [shown, setShown] = useState(toast);
  const [out, setOut] = useState(false);
  useEffect(() => {
    if (toast) {
      setShown(toast);
      setOut(false);
    } else if (shown) {
      setOut(true);
      const t = setTimeout(() => setShown(null), 220);
      return () => clearTimeout(t);
    }
  }, [toast, shown]);
  if (!shown) return null;
  return (
    <div
      key={shown.seq}
      className={`toast${shown.error ? " err" : ""}${out ? " out" : ""}`}
      role="status"
    >
      {shown.error ? <Icon.alert /> : <Icon.check />}
      <span>{shown.text}</span>
    </div>
  );
}

export function currentTabOrThrow() {
  const t = activeTab();
  if (!t) throw new Error("no document");
  return t;
}
