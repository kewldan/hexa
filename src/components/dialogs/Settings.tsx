// Settings (UI-11) including the theme editor (DISP-25) and key bindings (UI-03).
import { useEffect, useState } from "react";
import { bindings, COMMANDS } from "../../lib/commands";
import { chordOf, displayKeys } from "../../lib/keys";
import { encodings } from "../../state/actions";
import { type Column, DEFAULTS, type Settings, useSettings } from "../../state/settings";
import { closeDialog } from "../../state/ui";
import { ScriptSettings } from "../ScriptSettings";
import { Modal } from "./Dialogs";

const SECTIONS = [
  ["view", "Вид"],
  ["grid", "Сетка"],
  ["text", "Текст"],
  ["highlight", "Подсветка"],
  ["editor", "Редактор"],
  ["files", "Файлы"],
  ["theme", "Тема"],
  ["keys", "Клавиши"],
  ["access", "Доступность"],
  ["scripts", "Скрипты и API"],
  ["profiles", "Профили и JSON"],
] as const;

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="row-f">
      <span>{label}</span>
      {children}
    </div>
  );
}

function Select<T extends string | number>({
  value,
  options,
  onChange,
}: {
  value: T;
  options: [T, string][];
  onChange: (v: T) => void;
}) {
  return (
    <select
      className="inp"
      value={String(value)}
      onChange={(e) => {
        const raw = e.target.value;
        const hit = options.find(([v]) => String(v) === raw);
        if (hit) onChange(hit[0]);
      }}
    >
      {options.map(([v, l]) => (
        <option key={String(v)} value={String(v)}>
          {l}
        </option>
      ))}
    </select>
  );
}

function Check({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label className="check">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      {label}
    </label>
  );
}

const TOKENS = [
  "--ed",
  "--win",
  "--side",
  "--ink",
  "--muted",
  "--k0",
  "--kp",
  "--kc",
  "--kh",
  "--kf",
  "--mod",
  "--ins",
  "--match",
];

function KeysSection() {
  const s = useSettings((st) => st.s);
  const set = useSettings((st) => st.set);
  const [recording, setRecording] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const b = bindings();
  useEffect(() => {
    if (!recording) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") return setRecording(null);
      const chord = chordOf(e);
      if (!chord) return;
      set({ keyOverrides: { ...s.keyOverrides, [recording]: [chord] } });
      setRecording(null);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [recording, s.keyOverrides, set]);
  const list = COMMANDS.filter((c) =>
    `${c.group} ${c.title}`.toLowerCase().includes(filter.toLowerCase()),
  );
  return (
    <>
      <Row label="Пресет">
        <Select
          value={s.keymap}
          options={[
            ["hexa", "Hexa"],
            ["hxd", "HxD"],
            ["010", "010 Editor"],
            ["vim", "Vim (как bvi)"],
            ["emacs", "Emacs"],
          ]}
          onChange={(keymap) => set({ keymap })}
        />
      </Row>
      <div className="note">
        Нажмите на сочетание и введите новое; Esc — отмена. В пресете Vim сетка работает в режимах
        NORMAL / INSERT / REPLACE / VISUAL.
      </div>
      <input
        className="inp"
        placeholder="Фильтр команд"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        onKeyDown={(e) => e.stopPropagation()}
      />
      <div>
        {list.map((c) => (
          <div key={c.id} className="keyrow">
            <span>
              {c.title} <span className="mut">· {c.group}</span>
            </span>
            <button
              type="button"
              className={`btn${recording === c.id ? " rec" : ""}`}
              onClick={() => setRecording(c.id)}
              onContextMenu={(e) => {
                e.preventDefault();
                const o = { ...s.keyOverrides };
                delete o[c.id];
                set({ keyOverrides: o });
              }}
              title="Правый клик — сбросить"
            >
              {recording === c.id ? "Нажмите…" : displayKeys(b[c.id]?.join(" / ")) || "—"}
            </button>
          </div>
        ))}
      </div>
    </>
  );
}

function ProfilesSection() {
  const s = useSettings((st) => st.s);
  const set = useSettings((st) => st.set);
  const [name, setName] = useState("");
  const [json, setJson] = useState(() => JSON.stringify(s, null, 2));
  const [err, setErr] = useState("");
  return (
    <>
      <div className="pgh">Профили</div>
      <div style={{ display: "flex", gap: 6 }}>
        <input
          className="inp"
          placeholder="Имя профиля"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <button
          type="button"
          className="btn"
          disabled={!name.trim()}
          onClick={() => {
            const { profiles: _, ...rest } = s;
            set({ profiles: { ...s.profiles, [name.trim()]: rest } });
            setName("");
          }}
        >
          Сохранить текущие
        </button>
      </div>
      {Object.keys(s.profiles).map((n) => (
        <div key={n} style={{ display: "flex", gap: 6, alignItems: "center" }}>
          <span style={{ flex: 1 }}>{n}</span>
          <button
            type="button"
            className="btn pri"
            onClick={() => set({ ...s.profiles[n], profiles: s.profiles })}
          >
            Применить
          </button>
          <button
            type="button"
            className="btn"
            onClick={() => {
              const pr = { ...s.profiles };
              delete pr[n];
              set({ profiles: pr });
            }}
          >
            Удалить
          </button>
        </div>
      ))}
      <div className="pgh">settings.json</div>
      <textarea
        className={`inp${err ? " bad" : ""}`}
        style={{ minHeight: 220 }}
        value={json}
        onChange={(e) => setJson(e.target.value)}
        onKeyDown={(e) => e.stopPropagation()}
      />
      {err && <div className="err-text">{err}</div>}
      <div style={{ display: "flex", gap: 6 }}>
        <button
          type="button"
          className="btn pri"
          onClick={() => {
            try {
              const v = JSON.parse(json);
              if (typeof v !== "object" || v === null) throw new Error("ожидался объект");
              set(v);
              setErr("");
            } catch (e) {
              setErr(`Ошибка JSON: ${(e as Error).message}`);
            }
          }}
        >
          Применить JSON
        </button>
        <button type="button" className="btn" onClick={() => setJson(JSON.stringify(s, null, 2))}>
          Обновить из текущих
        </button>
      </div>
    </>
  );
}

export function SettingsDialog({ section }: { section?: string }) {
  const s = useSettings((st) => st.s);
  const set = useSettings((st) => st.set);
  const reset = useSettings((st) => st.reset);
  const [tab, setTab] = useState<string>(section ?? "view");
  const p =
    <K extends keyof Settings>(k: K) =>
    (v: Settings[K]) =>
      set({ [k]: v } as Partial<Settings>);
  const encs = encodings();
  const updateRule = (i: number, patch: Partial<{ from: number; to: number; color: string }>) =>
    set({ colorRules: s.colorRules.map((r, j) => (j === i ? { ...r, ...patch } : r)) });
  const loadTbl = async () => {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({
      multiple: false,
      filters: [{ name: "Character table", extensions: ["tbl", "txt"] }],
    });
    if (!path || Array.isArray(path)) return;
    const { api } = await import("../../ipc/api");
    const src = await api.readTextFile(path);
    const name = path.split(/[\\/]/).pop() ?? "table.tbl";
    set({ tblTables: { ...s.tblTables, [name]: src }, encoding: `tbl:${name}` });
  };
  const groups = [...new Set(encs.map((e) => e.group))];

  return (
    <Modal
      wide
      title="Настройки"
      footer={
        <>
          <button type="button" className="btn" style={{ marginRight: "auto" }} onClick={reset}>
            Сбросить всё
          </button>
          <button type="button" className="btn pri" onClick={closeDialog}>
            Готово
          </button>
        </>
      }
    >
      <div className="settings" style={{ margin: "0 -18px -14px" }}>
        <nav>
          {SECTIONS.map(([id, label]) => (
            <button
              type="button"
              key={id}
              className={tab === id ? "on" : ""}
              onClick={() => setTab(id)}
            >
              {label}
            </button>
          ))}
        </nav>
        <section key={tab}>
          {tab === "view" && (
            <>
              <Row label="Тема">
                <Select
                  value={s.theme}
                  options={[
                    ["system", "Как в системе"],
                    ["light", "Светлая"],
                    ["dark", "Тёмная"],
                    ["contrast", "Высокий контраст"],
                  ]}
                  onChange={p("theme")}
                />
              </Row>
              <Row label="Акцент">
                <div style={{ display: "flex", gap: 6 }}>
                  {["#2F6BF0", "#7C4DFF", "#E5603B", "#1F9D74", "#D6336C"].map((c) => (
                    <button
                      type="button"
                      key={c}
                      aria-label={c}
                      onClick={() => set({ accent: c })}
                      style={{
                        width: 24,
                        height: 24,
                        borderRadius: 7,
                        background: c,
                        boxShadow: s.accent === c ? `0 0 0 2px var(--ed), 0 0 0 4px ${c}` : "none",
                      }}
                    />
                  ))}
                  <input
                    type="color"
                    value={s.accent}
                    onChange={(e) => set({ accent: e.target.value })}
                    aria-label="Свой цвет"
                  />
                </div>
              </Row>
              <Row label="Плотность">
                <Select
                  value={s.density}
                  options={[
                    ["compact", "Компактно"],
                    ["regular", "Обычно"],
                    ["comfortable", "Просторно"],
                  ]}
                  onChange={p("density")}
                />
              </Row>
              <Row label="Шрифт сетки">
                <input
                  className="inp"
                  placeholder="по умолчанию: SF Mono / Cascadia / Menlo"
                  value={s.fontFamily}
                  onChange={(e) => set({ fontFamily: e.target.value })}
                />
              </Row>
              <Row label={`Размер шрифта · ${s.fontSize}px`}>
                <input
                  type="range"
                  min={10}
                  max={20}
                  step={0.5}
                  value={s.fontSize}
                  onChange={(e) => set({ fontSize: Number(e.target.value) })}
                />
              </Row>
              <Row label={`Межстрочный · ${s.lineSpacing.toFixed(2)}`}>
                <input
                  type="range"
                  min={0.85}
                  max={1.8}
                  step={0.05}
                  value={s.lineSpacing}
                  onChange={(e) => set({ lineSpacing: Number(e.target.value) })}
                />
              </Row>
              <Row label={`Масштаб · ${Math.round(s.zoom * 100)}%`}>
                <input
                  type="range"
                  min={0.6}
                  max={2}
                  step={0.05}
                  value={s.zoom}
                  onChange={(e) => set({ zoom: Number(e.target.value) })}
                />
              </Row>
              <Check
                label="Уменьшить анимации"
                checked={s.reduceMotion}
                onChange={p("reduceMotion")}
              />
              <Check
                label="Цвета для дальтоников (синий / оранжевый)"
                checked={s.colorBlind}
                onChange={p("colorBlind")}
              />
            </>
          )}
          {tab === "grid" && (
            <>
              <Row label="Байт в строке">
                <Select
                  value={s.bytesPerRow}
                  options={[
                    [8, "8"],
                    [16, "16"],
                    [24, "24"],
                    [32, "32"],
                    [64, "64"],
                    ["auto", "По ширине окна"],
                    ["pow2", "По ширине, степени двойки"],
                  ]}
                  onChange={p("bytesPerRow")}
                />
              </Row>
              <Row label="Группировка">
                <Select
                  value={s.group}
                  options={[
                    [1, "1"],
                    [2, "2"],
                    [4, "4"],
                    [8, "8"],
                    [16, "16"],
                  ]}
                  onChange={p("group")}
                />
              </Row>
              <Row label="Вид ячеек">
                <Select
                  value={s.cellView}
                  options={[
                    ["hex", "Hex"],
                    ["dec", "Десятичный"],
                    ["oct", "Восьмеричный"],
                    ["bin", "Двоичный"],
                    ["signed", "Знаковый"],
                    ["hexii", "HexII"],
                    ["float", "Float (ячейка 4 или 8 байт)"],
                    ["char", "Символы"],
                  ]}
                  onChange={p("cellView")}
                />
              </Row>
              <Row label="Ячейка">
                <div style={{ display: "flex", gap: 6 }}>
                  <Select
                    value={s.cellWidth}
                    options={[
                      [1, "byte"],
                      [2, "word"],
                      [4, "dword"],
                      [8, "qword"],
                    ]}
                    onChange={p("cellWidth")}
                  />
                  <Select
                    value={s.cellEndian}
                    options={[
                      ["little", "LE"],
                      ["big", "BE"],
                    ]}
                    onChange={p("cellEndian")}
                  />
                </div>
              </Row>
              <Check label="Строчные hex-цифры" checked={s.lowercase} onChange={p("lowercase")} />
              <Row label="Смещения">
                <Select
                  value={s.offsetBase}
                  options={[
                    ["hex", "Hex"],
                    ["dec", "Десятичные"],
                    ["oct", "Восьмеричные"],
                    ["seg", "Сегмент:смещение"],
                  ]}
                  onChange={p("offsetBase")}
                />
              </Row>
              {s.offsetBase === "seg" && (
                <Row label="Размер сегмента (hex)">
                  <input
                    className="inp"
                    value={s.offsetSegment.toString(16).toUpperCase()}
                    onChange={(e) => {
                      const v = Number.parseInt(e.target.value || "0", 16);
                      if (Number.isFinite(v) && v > 0) set({ offsetSegment: v });
                    }}
                  />
                </Row>
              )}
              <Row label="Базовый адрес (hex)">
                <input
                  className="inp"
                  value={s.baseAddress.toString(16).toUpperCase()}
                  onChange={(e) => {
                    const v = Number.parseInt(e.target.value || "0", 16);
                    if (Number.isFinite(v)) set({ baseAddress: v });
                  }}
                />
              </Row>
              <Check
                label="Смещения относительно курсора"
                checked={s.offsetRelative}
                onChange={p("offsetRelative")}
              />
              <Check
                label="Заголовок колонок"
                checked={s.showColHeader}
                onChange={p("showColHeader")}
              />
              <Row label="Колонки и порядок">
                <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                  {(["offset", "hex", "text"] as Column[]).map((c) => {
                    const on = s.columns.includes(c);
                    const label = c === "offset" ? "Смещение" : c === "hex" ? "Hex" : "Текст";
                    return (
                      <span key={c} style={{ display: "inline-flex", gap: 2 }}>
                        <button
                          type="button"
                          className={`chip${on ? " on" : ""}`}
                          onClick={() => {
                            const next = on ? s.columns.filter((x) => x !== c) : [...s.columns, c];
                            if (next.includes("hex") || next.includes("text"))
                              set({ columns: next });
                          }}
                        >
                          {label}
                        </button>
                        {on && s.columns.indexOf(c) > 0 && (
                          <button
                            type="button"
                            className="chip"
                            title="Левее"
                            onClick={() => {
                              const i = s.columns.indexOf(c);
                              const next = [...s.columns];
                              [next[i - 1], next[i]] = [next[i], next[i - 1]];
                              set({ columns: next });
                            }}
                          >
                            ←
                          </button>
                        )}
                      </span>
                    );
                  })}
                </div>
              </Row>
            </>
          )}
          {tab === "text" && (
            <>
              <Row label="Кодировка">
                <select
                  className="inp"
                  value={s.encoding}
                  onChange={(e) => set({ encoding: e.target.value })}
                >
                  {groups.map((g) => (
                    <optgroup key={g} label={g}>
                      {encs
                        .filter((e) => e.group === g)
                        .map((e) => (
                          <option key={e.id} value={e.id}>
                            {e.name}
                          </option>
                        ))}
                    </optgroup>
                  ))}
                  {Object.keys(s.tblTables).length > 0 && (
                    <optgroup label="Таблицы .tbl">
                      {Object.keys(s.tblTables).map((n) => (
                        <option key={n} value={`tbl:${n}`}>
                          {n}
                        </option>
                      ))}
                    </optgroup>
                  )}
                </select>
              </Row>
              <Row label="Таблицы .tbl">
                <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                  <button type="button" className="btn" onClick={loadTbl}>
                    Загрузить .tbl…
                  </button>
                  {Object.keys(s.tblTables).map((n) => (
                    <button
                      type="button"
                      key={n}
                      className="chip"
                      title="Удалить таблицу"
                      onClick={() => {
                        const t = { ...s.tblTables };
                        delete t[n];
                        set({
                          tblTables: t,
                          encoding: s.encoding === `tbl:${n}` ? "ascii" : s.encoding,
                        });
                      }}
                    >
                      {n} ×
                    </button>
                  ))}
                </div>
              </Row>
              <Row label="Непечатаемые">
                <Select
                  value={s.nonPrintable}
                  options={[
                    ["dot", "Точка ·"],
                    ["pictures", "Unicode Control Pictures ␀ ␊"],
                    ["blank", "Пусто"],
                  ]}
                  onChange={p("nonPrintable")}
                />
              </Row>
              <div className="note">
                Многобайтные символы UTF-8/16/32 рисуются в позиции первого байта.
              </div>
            </>
          )}
          {tab === "highlight" && (
            <>
              <Check
                label="Цвет по категориям байтов (0x00, печатные, управляющие, старшие, 0xFF)"
                checked={s.colorBytes}
                onChange={p("colorBytes")}
              />
              <Check
                label="Изменённые и вставленные байты, места удаления"
                checked={s.highlightChanges}
                onChange={p("highlightChanges")}
              />
              <Check label="Строка курсора" checked={s.highlightRow} onChange={p("highlightRow")} />
              <Check
                label="Колонка курсора"
                checked={s.highlightColumn}
                onChange={p("highlightColumn")}
              />
              <Check label="Зебра" checked={s.zebra} onChange={p("zebra")} />
              <Check
                label="Все вхождения байта под курсором"
                checked={s.highlightSame}
                onChange={p("highlightSame")}
              />
              <div className="pgh">Слои подсветки</div>
              {(
                [
                  ["changes", "Изменения"],
                  ["search", "Результаты поиска"],
                  ["marks", "Закладки и заметки"],
                  ["structure", "Поля структуры"],
                  ["diff", "Отличия при сравнении"],
                ] as const
              ).map(([k, label]) => (
                <Check
                  key={k}
                  label={label}
                  checked={s.layers[k]}
                  onChange={(v) => set({ layers: { ...s.layers, [k]: v } })}
                />
              ))}
              <div className="pgh">Свои цвета байтов</div>
              {s.colorRules.map((r, i) => (
                <div
                  key={`${r.from}-${r.to}-${r.color}`}
                  style={{ display: "flex", gap: 6, alignItems: "center" }}
                >
                  <span className="note">от</span>
                  <input
                    className="inp"
                    style={{ width: 60 }}
                    defaultValue={r.from.toString(16).toUpperCase()}
                    onBlur={(e) =>
                      updateRule(i, { from: Number.parseInt(e.target.value, 16) || 0 })
                    }
                  />
                  <span className="note">до</span>
                  <input
                    className="inp"
                    style={{ width: 60 }}
                    defaultValue={r.to.toString(16).toUpperCase()}
                    onBlur={(e) => updateRule(i, { to: Number.parseInt(e.target.value, 16) || 0 })}
                  />
                  <input
                    type="color"
                    value={r.color}
                    onChange={(e) => updateRule(i, { color: e.target.value })}
                  />
                  <button
                    type="button"
                    className="ib"
                    aria-label="Удалить правило"
                    onClick={() => set({ colorRules: s.colorRules.filter((_, j) => j !== i) })}
                  >
                    ×
                  </button>
                </div>
              ))}
              <button
                type="button"
                className="btn"
                onClick={() =>
                  set({ colorRules: [...s.colorRules, { from: 0x20, to: 0x7e, color: "#2f6bf0" }] })
                }
              >
                Добавить правило (hex-диапазон значений)
              </button>
            </>
          )}
          {tab === "access" && (
            <>
              <Row label={`Масштаб интерфейса · ${Math.round(s.uiScale * 100)}%`}>
                <input
                  type="range"
                  min={0.8}
                  max={1.6}
                  step={0.05}
                  value={s.uiScale}
                  onChange={(e) => set({ uiScale: Number(e.target.value) })}
                />
              </Row>
              <Check
                label="Озвучивать курсор и значения (screen reader)"
                checked={s.screenReader}
                onChange={p("screenReader")}
              />
              <Check
                label="Цвета подсветки, различимые при дальтонизме"
                checked={s.colorSafe}
                onChange={p("colorSafe")}
              />
              <Check
                label="Подсказка со значениями при наведении на байт"
                checked={s.hoverInspector}
                onChange={p("hoverInspector")}
              />
              <Row label="Язык интерфейса">
                <Select
                  value={s.locale}
                  options={[
                    ["ru", "Русский"],
                    ["en", "English"],
                  ]}
                  onChange={p("locale")}
                />
              </Row>
              <Check
                label="Проверять обновления (обращение к GitHub)"
                checked={s.checkUpdates}
                onChange={p("checkUpdates")}
              />
              <Check
                label="Локальный API для внешних инструментов (127.0.0.1)"
                checked={s.apiEnabled}
                onChange={p("apiEnabled")}
              />
              <div className="note">
                Данные не покидают компьютер. Сетевые функции выключены, пока вы их не включите.
              </div>
            </>
          )}
          {tab === "profiles" && <ProfilesSection />}
          {tab === "editor" && (
            <>
              <Row label="Режим при открытии">
                <Select
                  value={s.defaultMode}
                  options={[
                    ["overwrite", "Замена"],
                    ["insert", "Вставка"],
                    ["readOnly", "Только чтение"],
                  ]}
                  onChange={p("defaultMode")}
                />
              </Row>
              <Row label="Сохранение">
                <Select
                  value={s.save.strategy}
                  options={[
                    ["auto", "Авто: на месте при той же длине"],
                    ["atomic", "Всегда атомарно (временный файл)"],
                    ["inPlace", "Всегда на месте"],
                  ]}
                  onChange={(strategy) => set({ save: { ...s.save, strategy } })}
                />
              </Row>
              <Check
                label="Журнал записи на месте (восстановление при сбое, undo после сохранения)"
                checked={s.save.journal}
                onChange={(journal) => set({ save: { ...s.save, journal } })}
              />
              <Check
                label="Резервная копия .bak перед сохранением"
                checked={s.save.backup}
                onChange={(backup) => set({ save: { ...s.save, backup } })}
              />
              <Check
                label="Спрашивать при закрытии с несохранёнными правками"
                checked={s.confirmClose}
                onChange={p("confirmClose")}
              />
            </>
          )}
          {tab === "files" && (
            <>
              <Row label="Доступ других программ">
                <Select
                  value={s.share}
                  options={[
                    ["shared", "Без блокировки"],
                    ["denyWrite", "Запретить запись другим"],
                    ["exclusive", "Эксклюзивно"],
                  ]}
                  onChange={p("share")}
                />
              </Row>
              <div className="note">
                Файлы открываются лениво: читаются только видимые страницы, кэш до 128 МБ.
              </div>
            </>
          )}
          {tab === "theme" && (
            <>
              <div className="note">
                Переопределите цвета текущей темы. Пустое значение — цвет темы.
              </div>
              {TOKENS.map((t) => (
                <Row key={t} label={t}>
                  <div style={{ display: "flex", gap: 6 }}>
                    <input
                      type="color"
                      value={s.themeOverrides[t] ?? "#888888"}
                      onChange={(e) =>
                        set({ themeOverrides: { ...s.themeOverrides, [t]: e.target.value } })
                      }
                    />
                    <input
                      className="inp"
                      value={s.themeOverrides[t] ?? ""}
                      placeholder="из темы"
                      onChange={(e) => {
                        const o = { ...s.themeOverrides };
                        if (e.target.value) o[t] = e.target.value;
                        else delete o[t];
                        set({ themeOverrides: o });
                      }}
                    />
                  </div>
                </Row>
              ))}
              <button
                type="button"
                className="btn"
                onClick={() => set({ themeOverrides: DEFAULTS.themeOverrides })}
              >
                Сбросить цвета
              </button>
            </>
          )}
          {tab === "keys" && <KeysSection />}
          {tab === "scripts" && <ScriptSettings />}
        </section>
      </div>
    </Modal>
  );
}
