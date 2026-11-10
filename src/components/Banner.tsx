// Non-modal banners instead of dialogs (UI-07, FILE-05, FILE-06).
import { useState } from "react";
import * as A from "../state/actions";
import { type Tab, useTabs } from "../state/tabs";
import { Icon } from "./icons";

export function Banner({ tab }: { tab: Tab }) {
  const [closing, setClosing] = useState(false);
  const b = tab.banner;
  if (!b) return null;
  const dismiss = (after?: () => unknown) => {
    setClosing(true);
    setTimeout(() => {
      setClosing(false);
      useTabs.getState().update(tab.id, { banner: null });
      after?.();
    }, 200);
  };
  const warn = b.kind === "writeDenied" || b.kind === "journal";
  return (
    <div className={`banner${warn ? " warn" : ""}${closing ? " out" : ""}`} role="status">
      {warn ? <Icon.lock /> : <Icon.refresh />}
      <div className="bt">
        <b>{b.title}</b>
        {b.sub && <span>{b.sub}</span>}
      </div>
      {b.kind === "diskChanged" && (
        <>
          <button type="button" className="btn" onClick={() => dismiss(() => A.keepMine(tab.id))}>
            Оставить моё
          </button>
          {!tab.info.modified && (
            <button
              type="button"
              className="btn"
              onClick={() =>
                dismiss(() => A.setWatch(undefined, true).then(() => A.reload(tab.id)))
              }
            >
              Всегда перезагружать
            </button>
          )}
          <button type="button" className="btn pri" onClick={() => dismiss(() => A.reload(tab.id))}>
            Перезагрузить
          </button>
        </>
      )}
      {b.kind === "writeDenied" && (
        <>
          <button type="button" className="btn" onClick={() => dismiss()}>
            Понятно
          </button>
          <button
            type="button"
            className="btn pri"
            onClick={() => dismiss(() => A.setMode("overwrite").catch(() => {}))}
          >
            Править и сохранить с правами администратора
          </button>
        </>
      )}
      {(b.kind === "info" || b.kind === "journal") && (
        <button type="button" className="btn" onClick={() => dismiss()}>
          Закрыть
        </button>
      )}
    </div>
  );
}
