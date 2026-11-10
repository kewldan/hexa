// Clickable status bar (NAV-13, UI-06) with a live selection hash (ANL-06).

import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useEffect, useState } from "react";
import { api } from "../ipc/api";
import { runCommand } from "../lib/commands";
import { fmtSize, groupDec, hexN } from "../lib/format";
import { useVim } from "../lib/vim";
import * as A from "../state/actions";
import { useSettings } from "../state/settings";
import { hasSelection, selection, type Tab } from "../state/tabs";
import { openDialog, toast, useUi } from "../state/ui";

export function StatusBar({ tab }: { tab: Tab }) {
  const s = useSettings((st) => st.s);
  const set = useSettings((st) => st.set);
  const progress = useUi((u) => u.progress);
  const vimMode = useVim((v) => v.mode);
  const sel = selection(tab);
  const has = hasSelection(tab);
  const [crc, setCrc] = useState<string | null>(null);

  useEffect(() => {
    if (!has) return setCrc(null);
    const timer = setTimeout(() => {
      api
        .hashQuick(tab.id, sel.start, sel.end, "crc:CRC-32/ISO-HDLC")
        .then(setCrc)
        .catch(() => setCrc(null));
    }, 150);
    return () => clearTimeout(timer);
  }, [has, sel.start, sel.end, tab.id, tab.info.version]);

  const mode = tab.info.mode;
  const modeText = mode === "readOnly" ? "RO" : mode === "insert" ? "INS" : "OVR";
  const base = s.baseAddress;

  return (
    <footer className="sb">
      <button
        type="button"
        className="sbi"
        onClick={() => runCommand("nav.goto")}
        title="Перейти к смещению"
      >
        <span className="mut">Смещение</span>
        <span className="mono">0x{hexN(tab.cursor + base, 8)}</span>
        <span className="mono mut">{groupDec(tab.cursor + base)}</span>
      </button>
      {has && (
        <>
          <button
            type="button"
            className="sbi st"
            onClick={() => openDialog({ kind: "selectRange" })}
            title="Изменить выделение"
          >
            <span className="mut">Выделено</span>
            <span className="mono">
              {groupDec(sel.end - sel.start)} · 0x{hexN(sel.end - sel.start)}
            </span>
          </button>
          {crc && (
            <button
              type="button"
              className="sbi st"
              title="Живой хэш выделения — нажмите, чтобы скопировать"
              onClick={() => writeText(crc).then(() => toast(`CRC32 ${crc} скопирован`))}
            >
              <span className="mut">CRC32</span>
              <span className="mono">{crc}</span>
            </button>
          )}
        </>
      )}
      <div className="grow" />
      {progress && (
        <span className="sbi st">
          {progress.label}
          <span className="progress">
            <span
              style={{
                width: `${Math.round((progress.done / Math.max(1, progress.total)) * 100)}%`,
              }}
            />
          </span>
        </span>
      )}
      {tab.info.modified && (
        <span className="sbi st sb-opt">
          <span className="mut">Правки</span>
          <span className="mono">{groupDec(tab.info.pieceCount)} фрагм.</span>
        </span>
      )}
      {s.keymap === "vim" && <span className="sbi mode">{vimMode.toUpperCase()}</span>}
      {tab.info.path && (
        <button
          type="button"
          className={`sbi${tab.info.autoReload || tab.info.follow ? " on" : ""}`}
          onClick={() =>
            A.setWatch(
              tab.info.follow ? false : undefined,
              !(tab.info.autoReload || tab.info.follow),
            )
          }
          title="Автоперезагрузка: перечитывать файл, когда его меняет другой процесс. При несохранённых правках спросит. ⌥-клик — режим tail -f."
          onClickCapture={(e) => {
            if (e.altKey) {
              e.stopPropagation();
              A.setWatch(!tab.info.follow);
            }
          }}
        >
          <span className="live" aria-hidden="true" />
          {tab.info.follow ? "tail -f" : tab.info.autoReload ? "Авто" : "Ручн."}
        </button>
      )}
      {tab.info.fixedSize && (
        <button
          type="button"
          className="sbi sb-opt"
          onClick={A.toggleFixed}
          title="Размер файла зафиксирован"
        >
          FIX
        </button>
      )}
      <button
        type="button"
        className="sbi sb-opt"
        onClick={() => runCommand("view.encoding")}
        title="Кодировка текстовой колонки"
      >
        {A.encodingInfo(s.encoding).name}
      </button>
      <button
        type="button"
        className="sbi sb-opt"
        onClick={() => set({ inspectorEndian: s.inspectorEndian === "little" ? "big" : "little" })}
        title="Порядок байтов"
      >
        {s.inspectorEndian === "little" ? "LE" : "BE"}
      </button>
      <button
        type="button"
        className={`sbi mode${mode === "insert" ? " ins" : mode === "readOnly" ? " ro" : ""}`}
        onClick={A.toggleInsert}
        title="Режим ввода — клавиша Insert"
      >
        {modeText}
      </button>
      <span className="sbi mono mut" title={`${groupDec(tab.info.len)} байт`}>
        {fmtSize(tab.info.len)}
      </span>
    </footer>
  );
}
