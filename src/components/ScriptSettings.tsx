// Settings → «Скрипты и API»: local API (SCR-09), plugins (SCR-04) and the
// plugin catalog (SCR-05).
import { useEffect, useState } from "react";
import { type ApiStatus, type Catalog, scriptApi } from "../ipc/script";
import { refreshPlugins, useConsole } from "../lib/script";
import * as A from "../state/actions";
import { useSettings } from "../state/settings";
import { toast } from "../state/ui";
import "../styles/script.css";

function ApiSection() {
  const enabled = useSettings((s) => s.s.apiEnabled);
  const set = useSettings((s) => s.set);
  const [status, setStatus] = useState<ApiStatus | null>(null);
  const [port, setPort] = useState("");
  const [shown, setShown] = useState(false);

  const load = () =>
    scriptApi
      .apiStatus()
      .then((s) => {
        setStatus(s);
        setPort(String(s.port));
      })
      .catch(A.report);
  useEffect(() => {
    void load();
    // The bridge starts/stops the server when the toggle changes.
    const t = setTimeout(load, 400);
    return () => clearTimeout(t);
  }, [enabled]);

  const applyPort = async () => {
    const p = Number(port);
    if (!Number.isInteger(p) || p < 1 || p > 65535) return toast("Порт: 1…65535", true);
    try {
      setStatus(await scriptApi.apiSetPort(p));
    } catch (e) {
      A.report(e);
    }
  };
  const regenerate = async () => {
    try {
      setStatus(await scriptApi.apiRegenerate());
      toast("Новый токен создан");
    } catch (e) {
      A.report(e);
    }
  };
  const copy = async () => {
    if (!status) return;
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    await writeText(status.token).then(() => toast("Токен скопирован"), A.report);
  };

  return (
    <>
      <div className="cap">Локальный API</div>
      <label className="check">
        <input
          type="checkbox"
          checked={enabled}
          onChange={(e) => set({ apiEnabled: e.target.checked })}
        />
        HTTP API на 127.0.0.1 для внешних инструментов
      </label>
      <div className="row-f">
        <span>Порт</span>
        <div className="sc-token">
          <input
            className="inp"
            aria-label="Порт API"
            inputMode="numeric"
            value={port}
            onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))}
            onKeyDown={(e) => e.key === "Enter" && applyPort()}
          />
          <button type="button" className="btn" onClick={applyPort}>
            Применить
          </button>
        </div>
      </div>
      <div className="row-f">
        <span>Токен</span>
        <div className="sc-token">
          <input
            className="inp"
            aria-label="Токен API"
            readOnly
            type={shown ? "text" : "password"}
            value={status?.token ?? ""}
          />
          <button type="button" className="btn" onClick={() => setShown(!shown)}>
            {shown ? "Скрыть" : "Показать"}
          </button>
          <button type="button" className="btn" onClick={copy}>
            Копировать
          </button>
        </div>
      </div>
      <div className="row-f">
        <span>Состояние</span>
        <div className="sc-token">
          <span
            className={`note sc-status${status?.running ? " on" : status?.error ? " bad" : ""}`}
          >
            {status?.running
              ? `работает: ${status.url}`
              : status?.error
                ? status.error
                : "выключен"}
          </span>
          <div className="grow" />
          <button type="button" className="btn" onClick={regenerate}>
            Новый токен
          </button>
        </div>
      </div>
      <div className="note">
        POST JSON с заголовком <span className="mono">Authorization: Bearer &lt;токен&gt;</span>,
        например{" "}
        <span className="mono">{`{"id":1,"method":"readRange","params":{"offset":0,"len":16}}`}</span>
        . Методы: version, openFile, listDocuments, readRange, write, goto, find, runScript. Сервер
        слушает только 127.0.0.1 и отклоняет запросы из браузера.
      </div>
    </>
  );
}

function PluginsSection() {
  const listing = useConsole((c) => c.plugins);
  const [catalog, setCatalog] = useState<{ path: string; data: Catalog } | null>(null);
  useEffect(() => {
    void refreshPlugins();
  }, []);

  const openDir = () => scriptApi.pluginsOpenDir().catch(A.report);
  const pickCatalog = async () => {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({
      multiple: false,
      filters: [{ name: "Каталог Hexa", extensions: ["json"] }],
    });
    if (typeof path !== "string") return;
    try {
      setCatalog({ path, data: await scriptApi.catalogRead(path) });
    } catch (e) {
      A.report(e);
    }
  };
  const install = async (id: string) => {
    if (!catalog) return;
    try {
      useConsole.getState().set({ plugins: await scriptApi.catalogInstall(catalog.path, id) });
      toast(`Установлено: ${id}`);
    } catch (e) {
      A.report(e);
    }
  };

  return (
    <>
      <div className="cap">Плагины WASM</div>
      <div className="note mono">{listing?.dir ?? "…"}</div>
      {listing && listing.plugins.length > 0 ? (
        <div className="list">
          {listing.plugins.map((p) => (
            <div key={p.id} className="li" style={{ gridTemplateColumns: "minmax(0,1fr) auto" }}>
              <span className="t">
                <b>
                  {p.name} <span className="tag">{p.version}</span>
                </b>
                <span className="note">{p.description || p.id}</span>
              </span>
              <span className="tag">{p.kinds.join(", ")}</span>
            </div>
          ))}
        </div>
      ) : (
        <div className="note">Плагины не установлены.</div>
      )}
      {listing?.failures.map((f) => (
        <div key={f.path} className="err-text">
          {f.path.split(/[\\/]/).pop()}: {f.error}
        </div>
      ))}
      <div className="sc-row">
        <button type="button" className="btn" onClick={() => refreshPlugins(true)}>
          Перезагрузить
        </button>
        <button type="button" className="btn" onClick={openDir}>
          Открыть папку
        </button>
        <button type="button" className="btn" onClick={pickCatalog}>
          Установить из каталога…
        </button>
      </div>
      {catalog && (
        <div className="sc-catalog">
          <div className="note">
            Каталог: {catalog.path.split(/[\\/]/).pop()} — файлы проверяются по SHA-256.
          </div>
          {[...catalog.data.plugins, ...catalog.data.templates].map((e) => (
            <div key={e.id} className="li" style={{ gridTemplateColumns: "minmax(0,1fr) auto" }}>
              <span className="t">
                <b>
                  {e.name} <span className="tag">{e.version}</span>
                </b>
                <span className="note">
                  {e.kinds ? `плагин: ${e.kinds.join(", ")}` : "шаблон"}
                  {e.description ? ` · ${e.description}` : ""}
                </span>
              </span>
              <button type="button" className="chip" onClick={() => install(e.id)}>
                Установить
              </button>
            </div>
          ))}
        </div>
      )}
    </>
  );
}

export function ScriptSettings() {
  return (
    <>
      <ApiSection />
      <PluginsSection />
    </>
  );
}
