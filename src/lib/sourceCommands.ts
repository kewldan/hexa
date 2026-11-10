// Commands of the «Источники» and «Память» groups (disks, images, remote
// sources, archives, process memory).

import type { SourceDialogName } from "../components/dialogs/SourceDialogs";
import { src } from "../ipc/sources";
import * as A from "../state/actions";
import { type PanelId, settings, useSettings } from "../state/settings";
import { activeTab } from "../state/tabs";
import { openDialog, toast } from "../state/ui";
import type { Command } from "./commands";

const dlg = (name: SourceDialogName, path?: string) => () =>
  openDialog({ kind: "src", name, path });

function showPanel(id: PanelId) {
  const l = settings().layout;
  const set = useSettings.getState().setLayout;
  if (l.left.includes(id)) set({ leftOpen: true, leftActive: id });
  else set({ rightOpen: true, rightActive: id });
}

const S = "Источники";
const M = "Память";

export const SOURCE_COMMANDS: Command[] = [
  { id: "src.disk", title: "Открыть диск…", group: S, run: dlg("disk") },
  { id: "src.image", title: "Открыть образ диска…", group: S, run: dlg("image") },
  { id: "src.remote", title: "Открыть по адресу (HTTP, S3, SFTP)…", group: S, run: dlg("remote") },
  { id: "src.archive", title: "Открыть архив…", group: S, run: dlg("archive") },
  {
    id: "src.archiveSave",
    title: "Сохранить в архив",
    group: S,
    doc: true,
    run: async () => {
      const t = activeTab();
      if (!t) return;
      const info = await src.info(t.id);
      if (info?.kind !== "archiveEntry") return toast("Вкладка открыта не из архива", true);
      openDialog({ kind: "src", name: "archive", path: info.path ?? undefined });
    },
  },
  { id: "src.capture", title: "Захват сети (UDP/TCP)…", group: S, run: dlg("capture") },
  { id: "src.clone", title: "Клонировать / снять образ…", group: S, doc: true, run: dlg("clone") },
  { id: "src.wipe", title: "Затереть…", group: S, doc: true, run: dlg("wipe") },
  {
    id: "src.commit",
    title: "Записать изменения в источник…",
    group: S,
    doc: true,
    run: dlg("commit"),
  },
  { id: "src.raid", title: "Собрать RAID…", group: S, run: dlg("raid") },
  {
    id: "src.shred",
    title: "Уничтожить файл…",
    group: S,
    run: () => openDialog({ kind: "src", name: "shred" }),
  },
  {
    id: "src.streams",
    title: "Потоки файла (xattr / ADS)…",
    group: S,
    run: () => dlg("streams", activeTab()?.info.path ?? undefined)(),
  },
  { id: "view.disk", title: "Панель диска", group: "Вид", run: () => showPanel("disk") },
  { id: "mem.process", title: "Открыть процесс…", group: M, run: dlg("process") },
  {
    id: "mem.panel",
    title: "Панель памяти: регионы, сканер, указатели",
    group: M,
    run: () => showPanel("memory"),
  },
  {
    id: "mem.unfold",
    title: "Развернуть неотображённую память",
    group: M,
    doc: true,
    run: () => A.unfoldAll(),
  },
];
