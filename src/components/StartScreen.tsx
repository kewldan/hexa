// Start screen (UI-08): drop zone, recent and pinned files, quick actions.
import { useEffect, useMemo, useState } from "react";
import { api, type RecentItem, type RecoveryItem } from "../ipc/api";
import { keysFor } from "../lib/commands";
import { detectFormat } from "../lib/detect";
import { fmtSize, relTime } from "../lib/format";
import { displayKeys } from "../lib/keys";
import * as A from "../state/actions";
import { openDialog, useUi } from "../state/ui";
import { Icon } from "./icons";

/** A small sample to explore without a file at hand. */
function sampleBytes(): number[] {
  const png = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0,
    0x20, 0, 0, 0, 0x20, 8, 6, 0, 0, 0, 0x73, 0x7a, 0x7a, 0xf4,
  ];
  const text = Array.from(
    new TextEncoder().encode("Hexa — пример файла. Hello, hex! Привет, байты. "),
  );
  const out = [...png, ...text];
  for (let i = 0; out.length < 4096; i++) out.push(i % 7 === 0 ? 0 : (i * 37) & 0xff);
  return out;
}

export function StartScreen() {
  const [recent, setRecent] = useState<RecentItem[]>([]);
  const [recovery, setRecovery] = useState<RecoveryItem[]>([]);
  const [q, setQ] = useState("");
  const [popped, setPopped] = useState<string | null>(null);
  const over = useUi((u) => u.dragOver);

  const load = () => {
    api
      .recentList()
      .then(setRecent)
      .catch(() => {});
    api
      .recoveryList()
      .then(setRecovery)
      .catch(() => {});
  };
  useEffect(load, []);

  const filtered = useMemo(
    () => recent.filter((r) => `${r.name} ${r.dir}`.toLowerCase().includes(q.toLowerCase())),
    [recent, q],
  );
  const pinned = filtered.filter((r) => r.pinned);
  const rest = filtered.filter((r) => !r.pinned);

  const pin = async (r: RecentItem) => {
    await api.recentPin(r.path, !r.pinned);
    setPopped(r.path);
    load();
  };

  const item = (r: RecentItem, i: number) => (
    <div
      key={r.path}
      className={`ri${r.exists ? "" : " gone"}`}
      style={{ ["--dl" as string]: `${0.1 + i * 0.03}s` }}
    >
      <button
        type="button"
        className="rl"
        onClick={() => A.openPaths([r.path])}
        disabled={!r.exists}
        title={r.path}
      >
        <span className="fbadge">{detectFormat(r.head, r.name).badge}</span>
        <span className="rn">
          <b>{r.name}</b>
          <span>{r.dir}</span>
        </span>
        <span className="rm">
          <span className="mono">{r.size === null ? "нет файла" : fmtSize(r.size)}</span>
          <span className="rt">{relTime(r.openedMs)}</span>
        </span>
      </button>
      <button
        type="button"
        className={`pinb${r.pinned ? " on" : ""}${popped === r.path ? " pop" : ""}`}
        aria-label={r.pinned ? "Открепить" : "Закрепить"}
        title={r.pinned ? "Открепить" : "Закрепить"}
        onClick={() => pin(r)}
      >
        <Icon.star />
      </button>
    </div>
  );

  return (
    <div className="start">
      <div className="inner">
        <div className="col">
          <div className="hero">
            <div className="mark" aria-hidden="true">
              {Array.from({ length: 9 }, (_, i) => (
                // biome-ignore lint/suspicious/noArrayIndexKey: decorative cells
                <span key={i} />
              ))}
            </div>
            <div>
              <h1>Hexa</h1>
              <p>Файл любого размера открывается мгновенно</p>
            </div>
          </div>
          <div className={`drop${over ? " over" : ""}`}>
            <div className="dicon" aria-hidden="true">
              <Icon.upload />
            </div>
            <h2>{over ? "Отпустите, чтобы открыть" : "Перетащите файл сюда"}</h2>
            <span className="sub">
              Hexa читает только видимые байты, поэтому 100 ГБ открываются так же быстро, как 100
              байт. Исходный файл не меняется, пока вы не сохраните.
            </span>
            <div className="btns">
              <button type="button" className="btn lg pri" onClick={() => A.openWithDialog()}>
                Выбрать файл… <span className="kbd">{displayKeys(keysFor("file.open"))}</span>
              </button>
              <button
                type="button"
                className="btn lg"
                onClick={() => A.newDocument({ kind: "bytes", bytes: sampleBytes() })}
              >
                Открыть пример
              </button>
            </div>
          </div>
          <div className="qa">
            <button type="button" className="qb" onClick={() => openDialog({ kind: "newFile" })}>
              <span className="qi">
                <Icon.newFile />
              </span>
              <span className="qt">
                <b>Новый файл</b>
                <span>Пустой, заданного размера или из буфера</span>
              </span>
              <span className="kbd qk">{displayKeys(keysFor("file.new"))}</span>
            </button>
            <button type="button" className="qb" onClick={() => A.openWithDialog(true)}>
              <span className="qi">
                <Icon.lock />
              </span>
              <span className="qt">
                <b>Только для чтения</b>
                <span>Безопасный просмотр без случайных правок</span>
              </span>
            </button>
            <button
              type="button"
              className="qb"
              disabled
              title="Появится на этапе «Системный уровень»"
            >
              <span className="qi">
                <Icon.disk />
              </span>
              <span className="qt">
                <b>Диск или раздел</b>
                <span>Секторный режим, только чтение</span>
              </span>
              <span className="soon">скоро</span>
            </button>
            <button
              type="button"
              className="qb"
              disabled
              title="Появится на этапе «Продуктивность»"
            >
              <span className="qi">
                <Icon.compare />
              </span>
              <span className="qt">
                <b>Сравнить два файла</b>
                <span>Со сдвигами, 3-way merge</span>
              </span>
              <span className="soon">скоро</span>
            </button>
          </div>
        </div>
        <div className="col r">
          {recovery.length > 0 && (
            <div className="spanel">
              <div className="ph">
                <span className="cap">Несохранённые правки</span>
              </div>
              <div className="none" style={{ display: "flex", gap: 10, alignItems: "center" }}>
                <span style={{ flex: 1 }}>
                  После прошлого запуска остались правки в {recovery.length} файл
                  {recovery.length === 1 ? "е" : "ах"}.
                </span>
                <button
                  type="button"
                  className="btn pri"
                  onClick={() => openDialog({ kind: "recovery" })}
                >
                  Восстановить…
                </button>
              </div>
            </div>
          )}
          <div className="spanel">
            <label className="flt">
              <Icon.search />
              <span className="vh">Фильтр</span>
              <input
                value={q}
                onChange={(e) => setQ(e.target.value)}
                placeholder="Найти среди недавних"
                spellCheck={false}
                autoComplete="off"
                onKeyDown={(e) => e.stopPropagation()}
              />
            </label>
            {pinned.length > 0 && (
              <>
                <div className="ph">
                  <span className="cap">Закреплённые</span>
                </div>
                {pinned.map(item)}
              </>
            )}
            {rest.length > 0 && (
              <>
                <div className="ph">
                  <span className="cap">Недавние</span>
                </div>
                {rest.map((r, i) => item(r, i + pinned.length))}
              </>
            )}
            {filtered.length === 0 && (
              <div className="none">
                {q ? `Нет файлов по запросу «${q}»` : "Здесь появятся открытые файлы."}
              </div>
            )}
          </div>
          <div className="foot">
            <span>
              <span className="kbd">{displayKeys(keysFor("file.open"))}</span> открыть
            </span>
            <span>
              <span className="kbd">{displayKeys(keysFor("file.new"))}</span> новый
            </span>
            <span>
              <span className="kbd">{displayKeys(keysFor("view.palette"))}</span> команды
            </span>
            <span>Перетащите файл в любое место окна</span>
          </div>
        </div>
      </div>
    </div>
  );
}
