// Export of a range as firmware or a text dump, import of both (IO-01…04).
import { open as openFile, save as saveFile } from "@tauri-apps/plugin-dialog";
import { useState } from "react";
import { anl, type DumpFormat, type FirmwareFormat } from "../../ipc/analysis";
import { evalExpr } from "../../lib/expr";
import { fmtSize, hexN } from "../../lib/format";
import * as A from "../../state/actions";
import { hasSelection, selection, useActiveTab } from "../../state/tabs";
import { closeDialog, toast } from "../../state/ui";
import { Modal } from "./Dialogs";

type Fmt =
  | { group: "fw"; id: FirmwareFormat; name: string; ext: string }
  | { group: "dump"; id: DumpFormat["kind"]; name: string; ext: string };

const FORMATS: Fmt[] = [
  { group: "fw", id: "intelHex", name: "Intel HEX", ext: "hex" },
  { group: "fw", id: "sRecord", name: "Motorola S-Record", ext: "srec" },
  { group: "fw", id: "tiTxt", name: "TI-TXT", ext: "txt" },
  { group: "fw", id: "tekHex", name: "Tektronix Extended", ext: "tek" },
  { group: "dump", id: "xxd", name: "xxd", ext: "txt" },
  { group: "dump", id: "hexdumpC", name: "hexdump -C", ext: "txt" },
  { group: "dump", id: "html", name: "HTML", ext: "html" },
  { group: "dump", id: "rtf", name: "RTF", ext: "rtf" },
  { group: "dump", id: "markdown", name: "Markdown", ext: "md" },
  { group: "dump", id: "csv", name: "CSV", ext: "csv" },
  { group: "dump", id: "cHeader", name: "Массив C", ext: "h" },
];

export function ExportDialog() {
  const tab = useActiveTab();
  const [fmt, setFmt] = useState<Fmt>(FORMATS[0]);
  const [base, setBase] = useState("0");
  const [busy, setBusy] = useState(false);
  if (!tab) return null;
  const r = hasSelection(tab) ? selection(tab) : { start: 0, end: tab.info.len };
  const baseVal = (() => {
    try {
      return evalExpr(base, { cursor: tab.cursor, length: tab.info.len }).value;
    } catch {
      return null;
    }
  })();
  const run = async () => {
    const path = await saveFile({
      defaultPath: `${tab.info.name}.${fmt.ext}`,
      filters: [{ name: fmt.name, extensions: [fmt.ext] }],
    });
    if (!path) return;
    setBusy(true);
    try {
      if (fmt.group === "fw")
        await anl.fwExport(tab.id, r.start, r.end, baseVal ?? 0, fmt.id, path);
      else {
        const name = tab.info.name.replace(/[^A-Za-z0-9_]/g, "_") || "data";
        const f: DumpFormat =
          fmt.id === "hexdumpC"
            ? { kind: "hexdumpC", squeeze: true }
            : fmt.id === "markdown" || fmt.id === "csv"
              ? { kind: fmt.id, width: 16 }
              : fmt.id === "cHeader"
                ? { kind: "cHeader", name }
                : { kind: fmt.id };
        await anl.dumpExport(tab.id, r.start, r.end, f, path);
      }
      closeDialog();
      toast(`Сохранено: ${fmt.name}`);
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title="Экспорт"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            disabled={busy || (fmt.group === "fw" && baseVal === null)}
            onClick={run}
          >
            Сохранить…
          </button>
        </>
      }
    >
      <div className="note">
        {hasSelection(tab) ? "Выделение" : "Весь файл"}: 0x{hexN(r.start)}…0x{hexN(r.end)} ·{" "}
        {fmtSize(r.end - r.start)}
      </div>
      <div className="opt-list">
        {FORMATS.map((f) => (
          <button
            type="button"
            key={f.id}
            className={`opt${f.id === fmt.id ? " on" : ""}`}
            onClick={() => setFmt(f)}
          >
            <span />
            <span>
              <b>{f.name}</b>
              <span className="note">
                {" "}
                · {f.group === "fw" ? "прошивка с адресами" : "текстовый дамп"}
              </span>
            </span>
            <span />
          </button>
        ))}
      </div>
      {fmt.group === "fw" && (
        <div className="row-f">
          <span>Начальный адрес</span>
          <input
            className={`inp mono${baseVal === null ? " bad" : ""}`}
            value={base}
            onChange={(e) => setBase(e.target.value)}
            onKeyDown={(e) => e.stopPropagation()}
          />
        </div>
      )}
    </Modal>
  );
}

export async function importFirmware() {
  const path = await openFile({
    multiple: false,
    filters: [
      {
        name: "Прошивки",
        extensions: ["hex", "ihx", "srec", "s19", "s28", "s37", "mot", "txt", "tek"],
      },
    ],
  });
  if (typeof path !== "string") return;
  try {
    const r = await anl.fwImport(path);
    await A.adopt(r.info);
    toast(
      `Импорт ${r.format}: ${r.segments} сегм., база 0x${hexN(r.base)}${r.errors.length ? `, ошибок: ${r.errors.length} (строка ${r.errors[0].line})` : ""}`,
      r.errors.length > 0,
    );
  } catch (e) {
    A.report(e);
  }
}

export async function importDump() {
  const path = await openFile({
    multiple: false,
    filters: [{ name: "Дампы", extensions: ["txt", "hex", "dump", "xxd"] }],
  });
  if (typeof path !== "string") return;
  try {
    const r = await anl.dumpImport(path);
    await A.adopt(r.info);
    const bad = r.errors.length + r.warnings.length;
    toast(`Импорт дампа (${r.format})${bad ? `: замечаний ${bad}` : ""}`, r.errors.length > 0);
  } catch (e) {
    A.report(e);
  }
}
