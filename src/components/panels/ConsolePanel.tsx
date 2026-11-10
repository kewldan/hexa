// Lua console (SCR-02), script editor (SCR-01), macros (SCR-03) and plugin
// runs (SCR-04). State lives in `lib/script` so it survives panel switches.
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { scriptApi } from "../../ipc/script";
import { fmtSize } from "../../lib/format";
import {
  type ConsoleView,
  clearConsole,
  completeInput,
  evalLine,
  loadMacro,
  macroToLua,
  openScriptFile,
  print,
  refreshPlugins,
  replayLastMacro,
  resetRepl,
  runScript,
  runScriptFile,
  saveMacro,
  saveScriptFile,
  setSource,
  stopScript,
  toggleRecording,
  useConsole,
} from "../../lib/script";
import * as A from "../../state/actions";
import { hasSelection, selection, type Tab } from "../../state/tabs";
import { Icon } from "../icons";
import "../../styles/script.css";

const VIEWS: [ConsoleView, string][] = [
  ["repl", "Консоль"],
  ["script", "Скрипт"],
  ["macro", "Макросы"],
  ["plugins", "Плагины"],
];

function Log() {
  const lines = useConsole((c) => c.lines);
  const box = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = box.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);
  return (
    <div className="sc-log mono" ref={box} role="log" aria-label="Вывод консоли">
      {lines.length === 0 ? (
        <div className="sc-hint">
          Lua 5.4 с API <b>hexa</b>: hexa.read(0, 16), hexa.find_all("PNG"), hexa.hash("sha256")…
          Tab — дополнение, ↑/↓ — история, ⇧↵ — новая строка.
        </div>
      ) : (
        lines.map((l) => (
          <div key={l.id} className={`sc-l ${l.kind}`}>
            {l.text || " "}
          </div>
        ))
      )}
    </div>
  );
}

function ReplInput() {
  const pending = useConsole((c) => c.pending);
  const running = useConsole((c) => c.running);
  const [text, setText] = useState("");
  const [hist, setHist] = useState(-1);
  const input = useRef<HTMLTextAreaElement>(null);

  const submit = () => {
    const line = text;
    setText("");
    setHist(-1);
    void evalLine(line);
  };

  const onKey = async (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    e.stopPropagation();
    const el = e.currentTarget;
    const history = useConsole.getState().history;
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (!running) submit();
    } else if (e.key === "ArrowUp" && !el.value.slice(0, el.selectionStart).includes("\n")) {
      if (hist + 1 >= history.length) return;
      e.preventDefault();
      setHist(hist + 1);
      setText(history[hist + 1]);
    } else if (e.key === "ArrowDown" && !el.value.slice(el.selectionEnd).includes("\n")) {
      if (hist < 0) return;
      e.preventDefault();
      setHist(hist - 1);
      setText(hist - 1 >= 0 ? history[hist - 1] : "");
    } else if (e.key === "Tab") {
      e.preventDefault();
      const r = await completeInput(el.value, el.selectionStart);
      setText(r.text);
      requestAnimationFrame(() => input.current?.setSelectionRange(r.caret, r.caret));
      if (r.options.length > 1)
        print("info", r.options.slice(0, 40).join("  ") + (r.options.length > 40 ? "  …" : ""));
    } else if (e.key === "Escape" && running) {
      void stopScript();
    }
  };

  const rows = Math.min(6, text.split("\n").length);
  return (
    <div className={`sc-in${running ? " busy" : ""}`}>
      <span className="sc-ps mono" aria-hidden="true">
        {pending ? "»" : "›"}
      </span>
      <textarea
        ref={input}
        className="mono"
        aria-label="Lua-консоль"
        rows={rows}
        value={text}
        spellCheck={false}
        autoComplete="off"
        placeholder={pending ? "продолжение…" : "= 1 + 1"}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKey}
      />
    </div>
  );
}

function ScriptView() {
  const source = useConsole((c) => c.source);
  const name = useConsole((c) => c.scriptName);
  const running = useConsole((c) => c.running);
  return (
    <div className="sc-editor">
      <div className="sc-row">
        <span className="tag">{name}</span>
        <div className="grow" />
        <button type="button" className="chip" onClick={openScriptFile}>
          Открыть…
        </button>
        <button type="button" className="chip" onClick={saveScriptFile}>
          Сохранить…
        </button>
      </div>
      <textarea
        className="inp mono sc-src"
        aria-label="Текст скрипта"
        value={source}
        spellCheck={false}
        onChange={(e) => setSource(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            if (!running) void runScript(source);
          } else if (e.key === "Tab") {
            e.preventDefault();
            const el = e.currentTarget;
            const at = el.selectionStart;
            setSource(`${source.slice(0, at)}  ${source.slice(el.selectionEnd)}`);
            requestAnimationFrame(() => el.setSelectionRange(at + 2, at + 2));
          }
        }}
      />
      <div className="sc-row">
        {running ? (
          <button type="button" className="btn danger" onClick={stopScript}>
            <Icon.stop /> Стоп
          </button>
        ) : (
          <button type="button" className="btn pri" onClick={() => runScript(source)}>
            Запустить
          </button>
        )}
        <button type="button" className="btn" onClick={runScriptFile} disabled={running}>
          Запустить файл…
        </button>
      </div>
      <div className="note">
        ⌘/Ctrl+↵ — запуск. Глобальные переменные скрипта доступны в консоли.
      </div>
    </div>
  );
}

function MacroView() {
  const recording = useConsole((c) => c.recording);
  const last = useConsole((c) => c.lastMacro);
  const shown = recording ?? last;
  return (
    <div className="sc-macro">
      <div className="sc-row">
        <button
          type="button"
          className={`btn${recording ? " danger" : ""}`}
          onClick={toggleRecording}
          title="⌘⇧R / Ctrl+Shift+R"
        >
          {recording ? (
            <>
              <Icon.stop /> Остановить запись
            </>
          ) : (
            <>
              <span className="sc-rec" /> Записать
            </>
          )}
        </button>
        <button type="button" className="btn" onClick={replayLastMacro} disabled={!last}>
          Повторить
        </button>
      </div>
      <div className="sc-row">
        <button type="button" className="chip" onClick={saveMacro} disabled={!last}>
          Сохранить…
        </button>
        <button type="button" className="chip" onClick={loadMacro}>
          Загрузить…
        </button>
        <button type="button" className="chip" onClick={macroToLua} disabled={!last}>
          в Lua
        </button>
      </div>
      {shown ? (
        <>
          <div className="cap">
            {recording ? "Идёт запись" : shown.name} · {shown.steps.length} шаг(ов)
          </div>
          <ol className="sc-steps mono">
            {shown.steps.map((s, i) => (
              // biome-ignore lint/suspicious/noArrayIndexKey: steps are positional and may repeat
              <li key={i}>{s.command}</li>
            ))}
          </ol>
        </>
      ) : (
        <div className="empty">
          <span>
            Запишите последовательность команд (горячие клавиши, палитра, меню) и повторите её одним
            нажатием. Макрос можно сохранить в файл или превратить в Lua-скрипт.
          </span>
        </div>
      )}
    </div>
  );
}

function PluginsView({ tab }: { tab: Tab }) {
  const listing = useConsole((c) => c.plugins);
  const [request, setRequest] = useState("");
  useEffect(() => {
    if (!useConsole.getState().plugins) void refreshPlugins();
  }, []);
  const range = hasSelection(tab) ? selection(tab) : { start: 0, end: tab.info.len };

  const run = async (
    plugin: string,
    kind: "transform" | "inspector" | "formatter" | "provider",
    target?: "preview" | "newDoc",
  ) => {
    try {
      const out = await scriptApi.pluginsRun({
        plugin,
        kind,
        id: kind === "provider" ? null : tab.id,
        start: range.start,
        end: range.end,
        request,
        target,
      });
      for (const l of out.logs) print("debug", `[${plugin}] ${l}`);
      if (out.kind === "text") print("out", out.text);
      else if (out.kind === "rows")
        for (const r of out.rows) print("out", `${r.label}: ${r.value}`);
      else if (out.kind === "bytes")
        print(
          "out",
          `${fmtSize(out.len)}: ${out.preview
            .slice(0, 64)
            .map((b) => b.toString(16).padStart(2, "0"))
            .join(" ")}${out.len > 64 ? " …" : ""}`,
        );
      else if (out.kind === "doc") await A.adopt(out.info);
      else A.applyResult(tab.id, out.result);
      useConsole.getState().set({ view: "repl" });
    } catch (e) {
      A.report(e);
    }
  };

  return (
    <div className="sc-plugins">
      <div className="sc-row">
        <span className="tag">
          {hasSelection(tab) ? `выделение ${fmtSize(range.end - range.start)}` : "весь файл"}
        </span>
        <div className="grow" />
        <button type="button" className="chip" onClick={() => refreshPlugins(true)}>
          <Icon.refresh /> Перезагрузить
        </button>
      </div>
      {!listing || listing.plugins.length === 0 ? (
        <div className="empty">
          <span>
            Плагины WASM лежат в папке плагинов (см. Настройки → Скрипты и API). Каждый — папка с
            manifest.json и plugin.wasm.
          </span>
        </div>
      ) : (
        <div className="list">
          {listing.plugins.map((p) => (
            <div key={p.id} className="sc-plugin">
              <div>
                <b>{p.name}</b> <span className="tag">{p.version}</span>
              </div>
              {p.description && <div className="note">{p.description}</div>}
              <div className="sc-row">
                {p.kinds.includes("transform") && (
                  <>
                    <button
                      type="button"
                      className="chip"
                      onClick={() => run(p.id, "transform", "preview")}
                    >
                      Превью
                    </button>
                    <button
                      type="button"
                      className="chip"
                      onClick={() => run(p.id, "transform", "newDoc")}
                    >
                      В новую вкладку
                    </button>
                  </>
                )}
                {p.kinds.includes("inspector") && (
                  <button type="button" className="chip" onClick={() => run(p.id, "inspector")}>
                    Инспектор
                  </button>
                )}
                {p.kinds.includes("formatter") && (
                  <button type="button" className="chip" onClick={() => run(p.id, "formatter")}>
                    Формат
                  </button>
                )}
                {p.kinds.includes("provider") && (
                  <button type="button" className="chip" onClick={() => run(p.id, "provider")}>
                    Открыть источник
                  </button>
                )}
              </div>
            </div>
          ))}
          {listing.plugins.some((p) => p.kinds.includes("provider")) && (
            <label className="sc-field">
              <span className="note">Запрос для источника</span>
              <input
                className="inp"
                value={request}
                placeholder="uri или параметры"
                onChange={(e) => setRequest(e.target.value)}
                onKeyDown={(e) => e.stopPropagation()}
              />
            </label>
          )}
        </div>
      )}
      {listing && listing.failures.length > 0 && (
        <div className="sc-fail">
          {listing.failures.map((f) => (
            <div key={f.path} className="note">
              {f.path.split(/[\\/]/).pop()}: {f.error}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export function ConsolePanel({ tab }: { tab: Tab }) {
  const view = useConsole((c) => c.view);
  const running = useConsole((c) => c.running);
  const recording = useConsole((c) => c.recording);
  const set = useConsole((c) => c.set);
  return (
    <div className="sc">
      <div className="ph">
        <span className="cap">Lua</span>
        {running && <span className="tag sc-busy">выполняется…</span>}
        {recording && <span className="tag sc-recording">● запись {recording.steps.length}</span>}
        <div className="grow" />
        {running && (
          <button type="button" className="ib" aria-label="Остановить скрипт" onClick={stopScript}>
            <Icon.stop />
          </button>
        )}
        <button
          type="button"
          className="ib"
          aria-label="Сбросить состояние Lua"
          title="Сбросить глобальные переменные"
          onClick={resetRepl}
        >
          <Icon.refresh />
        </button>
        <button type="button" className="ib" aria-label="Очистить вывод" onClick={clearConsole}>
          <Icon.close />
        </button>
      </div>
      <div className="sc-views" role="tablist" aria-label="Разделы консоли">
        {VIEWS.map(([id, label]) => (
          <button
            type="button"
            role="tab"
            key={id}
            aria-selected={view === id}
            className={`chip${view === id ? " on" : ""}`}
            onClick={() => set({ view: id })}
          >
            {label}
          </button>
        ))}
      </div>
      {view === "script" && <ScriptView />}
      {view === "macro" && <MacroView />}
      {view === "plugins" && <PluginsView tab={tab} />}
      <Log />
      <ReplInput />
    </div>
  );
}
