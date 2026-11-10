// Command registry: every action reachable from keys, palette and menus (UI-02, UI-03).

import { getCurrentWindow } from "@tauri-apps/api/window";
import * as A from "../state/actions";
import * as D from "../state/diff";
import { type KeymapPreset, settings, useSettings } from "../state/settings";
import { activeTab, hasSelection, selectionOrByte } from "../state/tabs";
import { openDialog, toast, useUi } from "../state/ui";
import { normalizeBinding } from "./keys";
import { SOURCE_COMMANDS } from "./sourceCommands";

export interface Command {
  id: string;
  title: string;
  group: string;
  hint?: string;
  run: () => unknown;
  /** Needs an open document. */
  doc?: boolean;
  /** Needs a writable document. */
  write?: boolean;
  /** Hidden from the palette (navigation primitives). */
  hidden?: boolean;
}

const ui = () => useUi.getState();
const s = () => useSettings.getState();

function toggleSide(side: "left" | "right") {
  const l = settings().layout;
  s().setLayout(side === "left" ? { leftOpen: !l.leftOpen } : { rightOpen: !l.rightOpen });
}

function showPanel(id: import("../state/settings").PanelId) {
  const l = settings().layout;
  if (l.left.includes(id)) s().setLayout({ leftOpen: true, leftActive: id });
  else s().setLayout({ rightOpen: true, rightActive: id });
}

function zoom(delta: number) {
  const z = delta === 0 ? 1 : Math.min(2, Math.max(0.6, +(settings().zoom + delta).toFixed(2)));
  s().set({ zoom: z });
}

const THEMES = ["system", "light", "dark", "contrast"] as const;

export const COMMANDS: Command[] = [
  // Files
  { id: "file.open", title: "Открыть файл…", group: "Файл", run: () => A.openWithDialog() },
  {
    id: "file.openReadOnly",
    title: "Открыть только для чтения…",
    group: "Файл",
    run: () => A.openWithDialog(true),
  },
  {
    id: "file.new",
    title: "Новый файл…",
    group: "Файл",
    run: () => openDialog({ kind: "newFile" }),
  },
  {
    id: "file.newFromClipboard",
    title: "Новый файл из буфера обмена",
    group: "Файл",
    run: A.newFromClipboard,
  },
  { id: "file.save", title: "Сохранить", group: "Файл", doc: true, run: () => A.save() },
  { id: "file.saveAs", title: "Сохранить как…", group: "Файл", doc: true, run: () => A.saveAs() },
  {
    id: "file.saveCopy",
    title: "Сохранить копию…",
    group: "Файл",
    doc: true,
    run: () => A.saveCopy(false),
  },
  {
    id: "file.saveSelection",
    title: "Сохранить выделение как…",
    group: "Файл",
    doc: true,
    run: () => A.saveCopy(true),
  },
  {
    id: "file.saveElevated",
    title: "Сохранить с правами администратора",
    group: "Файл",
    doc: true,
    run: () => A.saveElevated(),
  },
  { id: "file.close", title: "Закрыть вкладку", group: "Файл", doc: true, run: () => A.closeTab() },
  {
    id: "file.reload",
    title: "Перечитать с диска",
    group: "Файл",
    doc: true,
    run: () => A.reload(),
  },
  {
    id: "file.props",
    title: "Свойства файла",
    group: "Файл",
    doc: true,
    run: () => openDialog({ kind: "props" }),
  },
  {
    id: "file.follow",
    title: "Следить за ростом файла (tail -f)",
    group: "Файл",
    doc: true,
    run: () => A.setWatch(!activeTab()?.info.follow),
  },
  {
    id: "file.autoReload",
    title: "Автоперезагрузка при изменении",
    group: "Файл",
    doc: true,
    run: () => A.setWatch(undefined, !activeTab()?.info.autoReload),
  },
  { id: "file.workspaceSave", title: "Сохранить проект…", group: "Файл", run: A.saveWorkspace },
  { id: "file.workspaceOpen", title: "Открыть проект…", group: "Файл", run: A.loadWorkspace },
  {
    id: "file.recovery",
    title: "Восстановление после сбоя…",
    group: "Файл",
    run: () => openDialog({ kind: "recovery" }),
  },
  {
    id: "window.new",
    title: "Новое окно",
    group: "Окно",
    run: () => import("../ipc/api").then(({ api }) => api.newWindow()),
  },
  {
    id: "window.tearOff",
    title: "Переместить вкладку в новое окно",
    group: "Окно",
    doc: true,
    run: () => A.tearOff(),
  },
  {
    id: "window.close",
    title: "Закрыть окно",
    group: "Окно",
    run: () => getCurrentWindow().close(),
  },
  { id: "tab.next", title: "Следующая вкладка", group: "Окно", run: () => cycleTab(1) },
  { id: "tab.prev", title: "Предыдущая вкладка", group: "Окно", run: () => cycleTab(-1) },

  // Edit
  { id: "edit.undo", title: "Отменить", group: "Правка", doc: true, run: A.undo },
  { id: "edit.redo", title: "Повторить", group: "Правка", doc: true, run: A.redo },
  { id: "edit.copy", title: "Копировать", group: "Правка", doc: true, run: () => A.copy() },
  {
    id: "edit.cut",
    title: "Вырезать",
    group: "Правка",
    doc: true,
    write: true,
    run: () => A.copy(undefined, true),
  },
  {
    id: "edit.paste",
    title: "Вставить",
    group: "Правка",
    doc: true,
    write: true,
    run: () => A.paste("auto"),
  },
  {
    id: "edit.pasteInsert",
    title: "Вставить со сдвигом",
    group: "Правка",
    doc: true,
    write: true,
    run: () => A.paste("insert"),
  },
  {
    id: "edit.pasteOverwrite",
    title: "Вставить с заменой",
    group: "Правка",
    doc: true,
    write: true,
    run: () => A.paste("overwrite"),
  },
  {
    id: "edit.pasteSpecial",
    title: "Вставить из… (формат буфера)",
    group: "Правка",
    doc: true,
    write: true,
    run: pasteSpecial,
  },
  {
    id: "edit.copyAs",
    title: "Копировать как…",
    group: "Правка",
    doc: true,
    run: () => openDialog({ kind: "copyAs" }),
  },
  {
    id: "edit.copyHex",
    title: "Копировать как hex-строку",
    group: "Правка",
    doc: true,
    run: () => A.copyAsText({ kind: "hexSpaced" }),
  },
  {
    id: "edit.copyC",
    title: "Копировать как массив C",
    group: "Правка",
    doc: true,
    run: () => A.copyAsText({ kind: "c" }),
  },
  {
    id: "edit.copyBase64",
    title: "Копировать как Base64",
    group: "Правка",
    doc: true,
    run: () => A.copyAsText({ kind: "base64" }),
  },
  {
    id: "edit.copyXxd",
    title: "Копировать как дамп xxd",
    group: "Правка",
    doc: true,
    run: () => {
      const t = activeTab();
      if (t) A.copyAsText({ kind: "xxd", offset: selectionOrByte(t).start });
    },
  },
  {
    id: "edit.delete",
    title: "Удалить",
    group: "Правка",
    doc: true,
    write: true,
    hidden: true,
    run: () => A.deleteBytes(false),
  },
  {
    id: "edit.backspace",
    title: "Удалить назад",
    group: "Правка",
    doc: true,
    write: true,
    hidden: true,
    run: () => A.deleteBytes(true),
  },
  {
    id: "edit.toggleInsert",
    title: "Режим: вставка / замена",
    group: "Правка",
    doc: true,
    run: A.toggleInsert,
  },
  {
    id: "edit.readOnly",
    title: "Режим: только чтение",
    group: "Правка",
    doc: true,
    run: () => A.setMode(activeTab()?.info.mode === "readOnly" ? "overwrite" : "readOnly"),
  },
  {
    id: "edit.fixedSize",
    title: "Фиксированный размер файла",
    group: "Правка",
    doc: true,
    run: A.toggleFixed,
  },
  {
    id: "edit.fill",
    title: "Заполнить выделение…",
    group: "Правка",
    doc: true,
    write: true,
    run: () => openDialog({ kind: "fill" }),
  },
  {
    id: "edit.insertBytes",
    title: "Вставить N байт…",
    group: "Правка",
    doc: true,
    write: true,
    run: () => openDialog({ kind: "insertBytes" }),
  },
  {
    id: "edit.insertFile",
    title: "Вставить файл…",
    group: "Правка",
    doc: true,
    write: true,
    run: A.insertFile,
  },
  {
    id: "edit.resize",
    title: "Изменить размер (обрезать / дополнить)…",
    group: "Правка",
    doc: true,
    write: true,
    run: () => openDialog({ kind: "resize" }),
  },
  {
    id: "edit.swap16",
    title: "Поменять порядок байт (16 бит)",
    group: "Правка",
    doc: true,
    write: true,
    run: () => swap(2),
  },
  {
    id: "edit.swap32",
    title: "Поменять порядок байт (32 бит)",
    group: "Правка",
    doc: true,
    write: true,
    run: () => swap(4),
  },
  {
    id: "edit.swap64",
    title: "Поменять порядок байт (64 бит)",
    group: "Правка",
    doc: true,
    write: true,
    run: () => swap(8),
  },
  {
    id: "edit.moveBlock",
    title: "Переместить выделение в…",
    group: "Правка",
    doc: true,
    write: true,
    run: () => openDialog({ kind: "moveBlock" }),
  },
  {
    id: "edit.reverse",
    title: "Развернуть выделение",
    group: "Правка",
    doc: true,
    write: true,
    run: () => {
      const t = activeTab();
      if (t) {
        const r = selectionOrByte(t);
        A.edit({ op: "reverse", start: r.start, end: r.end });
      }
    },
  },
  {
    id: "edit.zero",
    title: "Обнулить выделение",
    group: "Правка",
    doc: true,
    write: true,
    run: () => A.fillRanges({ kind: "byte", value: 0 }),
  },

  // Marks (NAV-14…18)
  {
    id: "marks.add",
    title: "Добавить закладку",
    group: "Закладки",
    doc: true,
    run: () => A.addMark("bookmark"),
  },
  {
    id: "marks.addLabel",
    title: "Добавить метку (имя для «Перейти»)",
    group: "Закладки",
    doc: true,
    run: () => A.addMark("label"),
  },
  {
    id: "marks.addNote",
    title: "Добавить заметку к выделению",
    group: "Закладки",
    doc: true,
    run: () => A.addMark("annotation"),
  },
  {
    id: "marks.next",
    title: "Следующая закладка",
    group: "Закладки",
    doc: true,
    run: () => A.nextMark(1),
  },
  {
    id: "marks.prev",
    title: "Предыдущая закладка",
    group: "Закладки",
    doc: true,
    run: () => A.nextMark(-1),
  },
  {
    id: "view.marks",
    title: "Панель: закладки и заметки",
    group: "Вид",
    run: () => showPanel("marks"),
  },
  {
    id: "view.layerChanges",
    title: "Слой: изменения",
    group: "Вид",
    run: () => toggleLayer("changes"),
  },
  {
    id: "view.layerSearch",
    title: "Слой: результаты поиска",
    group: "Вид",
    run: () => toggleLayer("search"),
  },
  {
    id: "view.layerMarks",
    title: "Слой: закладки и заметки",
    group: "Вид",
    run: () => toggleLayer("marks"),
  },
  {
    id: "view.layerStructure",
    title: "Слой: поля структуры",
    group: "Вид",
    run: () => toggleLayer("structure"),
  },
  { id: "view.layerDiff", title: "Слой: отличия", group: "Вид", run: () => toggleLayer("diff") },
  {
    id: "view.hoverInspector",
    title: "Подсказка со значениями при наведении",
    group: "Вид",
    run: () => s().set({ hoverInspector: !settings().hoverInspector }),
  },

  // Selection
  { id: "select.all", title: "Выделить всё", group: "Выделение", doc: true, run: A.selectAll },
  { id: "select.row", title: "Выделить строку", group: "Выделение", doc: true, run: A.selectRow },
  {
    id: "select.range",
    title: "Выделить диапазон…",
    group: "Выделение",
    doc: true,
    run: () => openDialog({ kind: "selectRange" }),
  },
  {
    id: "select.toZero",
    title: "Выделить до следующего нуля",
    group: "Выделение",
    doc: true,
    run: selectToZero,
  },

  // Navigation
  {
    id: "nav.goto",
    title: "Перейти к смещению…",
    group: "Переход",
    doc: true,
    run: () => ui().set({ goto: true }),
  },
  {
    id: "nav.back",
    title: "Назад по истории переходов",
    group: "Переход",
    doc: true,
    run: A.navBack,
  },
  {
    id: "nav.forward",
    title: "Вперёд по истории переходов",
    group: "Переход",
    doc: true,
    run: A.navForward,
  },
  {
    id: "nav.follow",
    title: "Перейти по указателю…",
    group: "Переход",
    doc: true,
    run: () => openDialog({ kind: "followPointer" }),
  },
  {
    id: "nav.follow32",
    title: "Перейти по указателю u32 LE",
    group: "Переход",
    doc: true,
    run: () => A.followPointer(32, "little"),
  },
  {
    id: "nav.nextChange",
    title: "Следующее изменение",
    group: "Переход",
    doc: true,
    run: () => A.nextChange(1),
  },
  {
    id: "nav.prevChange",
    title: "Предыдущее изменение",
    group: "Переход",
    doc: true,
    run: () => A.nextChange(-1),
  },
  {
    id: "nav.nextZero",
    title: "Следующий блок нулей",
    group: "Переход",
    doc: true,
    run: () => A.nextBlock("zero"),
  },
  {
    id: "nav.nextNonZero",
    title: "Следующий ненулевой байт",
    group: "Переход",
    doc: true,
    run: () => A.nextBlock("nonzero"),
  },
  {
    id: "nav.nextDifferent",
    title: "Следующий байт, отличный от текущего",
    group: "Переход",
    doc: true,
    run: () => A.nextBlock("different"),
  },
  {
    id: "nav.start",
    title: "В начало файла",
    group: "Переход",
    doc: true,
    hidden: true,
    run: () => A.moveHome(false, true),
  },
  {
    id: "nav.end",
    title: "В конец файла",
    group: "Переход",
    doc: true,
    hidden: true,
    run: () => A.moveEnd(false, true),
  },
  {
    id: "cursor.left",
    title: "Влево",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveBy(-1, false),
  },
  {
    id: "cursor.right",
    title: "Вправо",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveBy(1, false),
  },
  {
    id: "cursor.up",
    title: "Вверх",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveRows(-1, false),
  },
  {
    id: "cursor.down",
    title: "Вниз",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveRows(1, false),
  },
  {
    id: "cursor.pageUp",
    title: "Страница вверх",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveRows(-(A.viewport.rows - 1), false),
  },
  {
    id: "cursor.pageDown",
    title: "Страница вниз",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveRows(A.viewport.rows - 1, false),
  },
  {
    id: "cursor.home",
    title: "В начало строки",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveHome(false),
  },
  {
    id: "cursor.end",
    title: "В конец строки",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveEnd(false),
  },
  {
    id: "select.left",
    title: "Выделить влево",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveBy(-1, true),
  },
  {
    id: "select.right",
    title: "Выделить вправо",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveBy(1, true),
  },
  {
    id: "select.up",
    title: "Выделить вверх",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveRows(-1, true),
  },
  {
    id: "select.down",
    title: "Выделить вниз",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveRows(1, true),
  },
  {
    id: "select.pageUp",
    title: "Выделить страницу вверх",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveRows(-(A.viewport.rows - 1), true),
  },
  {
    id: "select.pageDown",
    title: "Выделить страницу вниз",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveRows(A.viewport.rows - 1, true),
  },
  {
    id: "select.home",
    title: "Выделить до начала строки",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveHome(true),
  },
  {
    id: "select.end",
    title: "Выделить до конца строки",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveEnd(true),
  },
  {
    id: "select.start",
    title: "Выделить до начала файла",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveHome(true, true),
  },
  {
    id: "select.fileEnd",
    title: "Выделить до конца файла",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveEnd(true, true),
  },
  {
    id: "cursor.nibbleLeft",
    title: "На полубайт влево",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveNibble(-1),
  },
  {
    id: "cursor.nibbleRight",
    title: "На полубайт вправо",
    group: "Курсор",
    doc: true,
    hidden: true,
    run: () => A.moveNibble(1),
  },
  {
    id: "cursor.switchColumn",
    title: "Переключить колонку hex / текст",
    group: "Курсор",
    doc: true,
    run: A.switchColumn,
  },

  // Search
  {
    id: "find.open",
    title: "Найти…",
    group: "Поиск",
    doc: true,
    run: () => ui().set({ find: true, findFocus: ui().findFocus + 1 }),
  },
  { id: "find.next", title: "Найти далее", group: "Поиск", doc: true, run: () => A.stepResult(1) },
  { id: "find.prev", title: "Найти назад", group: "Поиск", doc: true, run: () => A.stepResult(-1) },
  {
    id: "find.clear",
    title: "Сбросить результаты поиска",
    group: "Поиск",
    doc: true,
    run: A.clearResults,
  },

  // View
  {
    id: "view.palette",
    title: "Командная палитра",
    group: "Вид",
    run: () => ui().set({ palette: !ui().palette }),
  },
  { id: "view.left", title: "Левая панель", group: "Вид", run: () => toggleSide("left") },
  {
    id: "view.right",
    title: "Правая панель (инспектор)",
    group: "Вид",
    run: () => toggleSide("right"),
  },
  {
    id: "view.history",
    title: "Панель: история правок",
    group: "Вид",
    run: () => showPanel("history"),
  },
  {
    id: "view.hashes",
    title: "Панель: хэши и контрольные суммы",
    group: "Вид",
    run: () => showPanel("hashes"),
  },
  {
    id: "view.results",
    title: "Панель: результаты поиска",
    group: "Вид",
    run: () => showPanel("results"),
  },
  {
    id: "view.inspector",
    title: "Панель: инспектор данных",
    group: "Вид",
    run: () => showPanel("inspector"),
  },
  {
    id: "view.theme",
    title: "Сменить тему",
    group: "Вид",
    run: () => {
      const i = THEMES.indexOf(settings().theme);
      s().set({ theme: THEMES[(i + 1) % THEMES.length] });
    },
  },
  { id: "view.zoomIn", title: "Увеличить шрифт", group: "Вид", run: () => zoom(0.1) },
  { id: "view.zoomOut", title: "Уменьшить шрифт", group: "Вид", run: () => zoom(-0.1) },
  { id: "view.zoomReset", title: "Обычный размер шрифта", group: "Вид", run: () => zoom(0) },
  {
    id: "view.encoding",
    title: "Кодировка текстовой колонки…",
    group: "Вид",
    run: () => openDialog({ kind: "settings", section: "text" }),
  },
  {
    id: "view.endian",
    title: "Порядок байтов LE / BE",
    group: "Вид",
    run: () =>
      s().set({ inspectorEndian: settings().inspectorEndian === "little" ? "big" : "little" }),
  },
  {
    id: "view.offsetBase",
    title: "Смещения: hex / dec / oct / сегмент:смещение",
    group: "Вид",
    run: () => {
      const o = settings().offsetBase;
      s().set({
        offsetBase: o === "hex" ? "dec" : o === "dec" ? "oct" : o === "oct" ? "seg" : "hex",
      });
    },
  },
  {
    id: "view.cellView",
    title: "Вид ячеек: hex / dec / oct / bin",
    group: "Вид",
    run: cycleCellView,
  },
  {
    id: "view.bpr8",
    title: "8 байт в строке",
    group: "Вид",
    run: () => s().set({ bytesPerRow: 8 }),
  },
  {
    id: "view.bpr16",
    title: "16 байт в строке",
    group: "Вид",
    run: () => s().set({ bytesPerRow: 16 }),
  },
  {
    id: "view.bpr32",
    title: "32 байта в строке",
    group: "Вид",
    run: () => s().set({ bytesPerRow: 32 }),
  },
  {
    id: "view.bprAuto",
    title: "Байт в строке — по ширине окна",
    group: "Вид",
    run: () => s().set({ bytesPerRow: "auto" }),
  },
  {
    id: "view.settings",
    title: "Настройки…",
    group: "Вид",
    run: () => openDialog({ kind: "settings" }),
  },
  {
    id: "view.keys",
    title: "Горячие клавиши…",
    group: "Вид",
    run: () => openDialog({ kind: "settings", section: "keys" }),
  },
  { id: "view.layoutSave", title: "Сохранить раскладку панелей", group: "Вид", run: saveLayout },
  {
    id: "view.layoutReset",
    title: "Сбросить раскладку панелей",
    group: "Вид",
    run: () => import("../state/settings").then((m) => s().set({ layout: m.DEFAULT_LAYOUT })),
  },
  {
    id: "view.split2",
    title: "Разделить файл на 2 панели",
    group: "Вид",
    doc: true,
    run: () => A.splitPanes(2),
  },
  {
    id: "view.split3",
    title: "Разделить файл на 3 панели",
    group: "Вид",
    doc: true,
    run: () => A.splitPanes(3),
  },
  {
    id: "view.split4",
    title: "Разделить файл на 4 панели",
    group: "Вид",
    doc: true,
    run: () => A.splitPanes(4),
  },
  {
    id: "view.unsplit",
    title: "Убрать разделение",
    group: "Вид",
    doc: true,
    run: () => A.splitPanes(1),
  },
  {
    id: "view.sideBySide",
    title: "Открыть рядом с другой вкладкой…",
    group: "Вид",
    doc: true,
    run: () => openDialog({ kind: "pickTab" }),
  },
  {
    id: "view.foldSelection",
    title: "Свернуть выделенные строки",
    group: "Вид",
    doc: true,
    run: A.foldSelection,
  },
  {
    id: "view.foldRepeats",
    title: "Свернуть повторяющиеся строки",
    group: "Вид",
    doc: true,
    run: A.foldRepeats,
  },
  { id: "view.unfoldAll", title: "Развернуть всё", group: "Вид", doc: true, run: A.unfoldAll },
  {
    id: "diff.compare",
    title: "Сравнить с другой вкладкой…",
    group: "Сравнение",
    doc: true,
    run: () => openDialog({ kind: "pickTab", mode: "diff" }),
  },
  { id: "diff.next", title: "Следующее отличие", group: "Сравнение", run: () => D.nextRegion(1) },
  { id: "diff.prev", title: "Предыдущее отличие", group: "Сравнение", run: () => D.nextRegion(-1) },
  {
    id: "diff.takeLeft",
    title: "Взять отличие справа",
    group: "Сравнение",
    run: () => D.applyRegion(true),
  },
  {
    id: "diff.giveRight",
    title: "Отдать отличие направо",
    group: "Сравнение",
    run: () => D.applyRegion(false),
  },
  {
    id: "diff.takeAll",
    title: "Взять все отличия справа",
    group: "Сравнение",
    run: () => D.applyRegion(true, true),
  },
  {
    id: "diff.export",
    title: "Экспорт отчёта о сравнении…",
    group: "Сравнение",
    run: D.exportDiff,
  },
  {
    id: "diff.close",
    title: "Закончить сравнение",
    group: "Сравнение",
    run: () => {
      D.stopDiff();
      ui().set({ compare: null });
    },
  },
  {
    id: "diff.nway",
    title: "Сравнить несколько файлов…",
    group: "Сравнение",
    run: () => openDialog({ kind: "nway" }),
  },
  {
    id: "diff.merge3",
    title: "Трёхстороннее слияние…",
    group: "Сравнение",
    run: () => openDialog({ kind: "merge3" }),
  },
  {
    id: "patch.create",
    title: "Создать патч (IPS/UPS/BPS/VCDIFF/bsdiff)…",
    group: "Сравнение",
    run: () => openDialog({ kind: "patchCreate" }),
  },
  {
    id: "patch.apply",
    title: "Применить патч…",
    group: "Сравнение",
    doc: true,
    run: () => openDialog({ kind: "patchApply" }),
  },
  { id: "view.diff", title: "Панель сравнения", group: "Вид", run: () => showPanel("diff") },
  {
    id: "file.exportAs",
    title: "Экспорт: Intel HEX, S-Record, xxd, HTML, C…",
    group: "Файл",
    doc: true,
    run: () => openDialog({ kind: "exportAs" }),
  },
  {
    id: "file.importFirmware",
    title: "Импорт прошивки (Intel HEX, S-Record, TI-TXT, Tek)…",
    group: "Файл",
    run: () => import("../components/dialogs/IoDialogs").then((m) => m.importFirmware()),
  },
  {
    id: "file.importDump",
    title: "Импорт текстового дампа (xxd, hexdump, od)…",
    group: "Файл",
    run: () => import("../components/dialogs/IoDialogs").then((m) => m.importDump()),
  },
  { id: "view.analysis", title: "Панель анализа", group: "Вид", run: () => showPanel("analysis") },
  { id: "view.strings", title: "Панель строк", group: "Вид", run: () => showPanel("strings") },
  {
    id: "tools.calc",
    title: "Калькулятор и системы счисления",
    group: "Инструменты",
    hint: "BigInt, hex/dec/oct/bin, знаковые и float-представления",
    run: () => openDialog({ kind: "calc" }),
  },
  {
    id: "help.commands",
    title: "Справка по командам",
    group: "Справка",
    hint: "все команды с сочетаниями клавиш",
    run: () => openDialog({ kind: "help" }),
  },
  {
    id: "help.update",
    title: "Проверить обновления",
    group: "Справка",
    run: () => import("../components/dialogs/ToolDialogs").then((m) => m.checkUpdates(true)),
  },
  {
    id: "edit.clipHistory",
    title: "История буфера обмена…",
    group: "Правка",
    hint: "последние 20 копирований этой сессии",
    run: () => openDialog({ kind: "clipHistory" }),
  },
  {
    id: "file.print",
    title: "Печать…",
    group: "Файл",
    doc: true,
    run: () => openDialog({ kind: "print" }),
  },
  {
    id: "search.inFiles",
    title: "Найти в файлах и вкладках…",
    group: "Поиск",
    run: () => openDialog({ kind: "findInFiles" }),
  },
  {
    id: "file.split",
    title: "Разделить файл на части…",
    group: "Файл",
    doc: true,
    run: () => openDialog({ kind: "split" }),
  },
  {
    id: "file.concat",
    title: "Склеить файлы…",
    group: "Файл",
    run: () => import("../components/dialogs/ToolDialogs").then((m) => m.concatFiles()),
  },
  {
    id: "file.compareDisk",
    title: "Сравнить с версией на диске",
    group: "Сравнение",
    doc: true,
    run: () => A.compareWithDisk(),
  },
  {
    id: "view.shareLink",
    title: "Поделиться видом (ссылка)…",
    group: "Вид",
    doc: true,
    run: () => openDialog({ kind: "viewLink" }),
  },
  // Structures (STRC, NAV-12)
  {
    id: "struct.expand",
    title: "Расширить выделение до поля, затем структуры",
    group: "Структура",
    doc: true,
    run: () => import("../state/templates").then((m) => m.expandSelection()),
  },
  {
    id: "struct.shrink",
    title: "Сузить выделение обратно",
    group: "Структура",
    doc: true,
    run: () => import("../state/templates").then((m) => m.shrinkSelection()),
  },
  {
    id: "struct.auto",
    title: "Определить формат и применить шаблон",
    group: "Структура",
    doc: true,
    run: () => {
      const t = activeTab();
      if (t) void import("../state/templates").then((m) => m.autoApply(t.id));
      showPanel("structure");
    },
  },
  {
    id: "view.structure",
    title: "Панель структуры",
    group: "Вид",
    run: () => showPanel("structure"),
  },
  // Scripts (SCR-01…04)
  {
    id: "script.console",
    title: "Открыть консоль Lua",
    group: "Скрипты",
    run: () => showPanel("console"),
  },
  {
    id: "script.runFile",
    title: "Запустить скрипт из файла…",
    group: "Скрипты",
    run: () => import("./script").then((m) => m.runScriptFile()),
  },
  {
    id: "script.stop",
    title: "Остановить скрипт",
    group: "Скрипты",
    run: () => import("./script").then((m) => m.stopScript()),
  },
  {
    id: "macro.record",
    title: "Начать или остановить запись макроса",
    group: "Скрипты",
    run: () => import("./script").then((m) => m.toggleRecording()),
  },
  {
    id: "macro.replay",
    title: "Повторить последний макрос",
    group: "Скрипты",
    run: () => import("./script").then((m) => m.replayLastMacro()),
  },
  {
    id: "script.reloadPlugins",
    title: "Перезагрузить плагины",
    group: "Скрипты",
    run: () => import("./script").then((m) => m.reloadPlugins()),
  },
  {
    id: "help.about",
    title: "О программе",
    group: "Справка",
    run: () => openDialog({ kind: "about" }),
  },
  ...SOURCE_COMMANDS,
];

function toggleLayer(k: keyof ReturnType<typeof settings>["layers"]) {
  const l = settings().layers;
  s().set({ layers: { ...l, [k]: !l[k] } });
  toast(`Слой «${k}» ${l[k] ? "скрыт" : "показан"}`);
}

function cycleTab(dir: number) {
  import("../state/tabs").then(({ useTabs }) => {
    const st = useTabs.getState();
    if (st.tabs.length === 0) return;
    const i = st.tabs.findIndex((t) => t.id === st.active);
    st.activate(st.tabs[(i + dir + st.tabs.length) % st.tabs.length].id);
  });
}

function cycleCellView() {
  const order = ["hex", "dec", "oct", "bin", "signed", "hexii", "float", "char"] as const;
  const i = order.indexOf(settings().cellView);
  s().set({ cellView: order[(i + 1) % order.length] });
}

function swap(width: number) {
  const t = activeTab();
  if (!t) return;
  if (!hasSelection(t)) return toast("Выделите байты", true);
  const r = selectionOrByte(t);
  A.edit({ op: "byteSwap", start: r.start, end: r.end, width });
}

async function selectToZero() {
  const t = activeTab();
  if (!t) return;
  const { api } = await import("../ipc/api");
  const m = await api.searchNext(
    t.id,
    { kind: "hex", pattern: "00" },
    null,
    t.cursor,
    false,
    false,
  );
  A.selectRange(t.cursor, m ? m.offset : t.info.len);
}

async function pasteSpecial() {
  const { readText } = await import("@tauri-apps/plugin-clipboard-manager");
  const text = (await readText().catch(() => "")) ?? "";
  if (!text) return toast("Буфер обмена пуст", true);
  openDialog({ kind: "paste", text });
}

function saveLayout() {
  const name = `Раскладка ${Object.keys(settings().layouts).length + 1}`;
  s().set({ layouts: { ...settings().layouts, [name]: settings().layout } });
  toast(`Сохранено: ${name}`);
}

export function layoutCommands(): Command[] {
  return Object.entries(settings().layouts).map(([name, layout]) => ({
    id: `layout.${name}`,
    title: `Раскладка: ${name}`,
    group: "Вид",
    run: () => s().set({ layout }),
  }));
}

export function commandById(id: string): Command | undefined {
  return COMMANDS.find((c) => c.id === id);
}

// ---------------------------------------------------------------- keymaps

type Keymap = Record<string, string[]>;

const BASE: Keymap = {
  "file.open": ["Mod+O"],
  "file.new": ["Mod+N"],
  "file.save": ["Mod+S"],
  "file.saveAs": ["Shift+Mod+S"],
  "file.close": ["Mod+W"],
  "window.new": ["Shift+Mod+N"],
  "tab.next": ["Ctrl+Tab", "Mod+Alt+Right"],
  "tab.prev": ["Ctrl+Shift+Tab", "Mod+Alt+Left"],
  "edit.undo": ["Mod+Z"],
  "edit.redo": ["Shift+Mod+Z", "Mod+Y"],
  "edit.copy": ["Mod+C"],
  "edit.cut": ["Mod+X"],
  "edit.paste": ["Mod+V"],
  "edit.pasteSpecial": ["Shift+Mod+V"],
  "edit.copyAs": ["Shift+Mod+C"],
  "edit.delete": ["Delete"],
  "edit.backspace": ["Backspace"],
  "edit.toggleInsert": ["Insert"],
  "select.all": ["Mod+A"],
  "nav.goto": ["Mod+L"],
  "nav.back": ["Mod+["],
  "nav.forward": ["Mod+]"],
  "nav.follow32": ["Mod+J"],
  "nav.start": ["Mod+Home", "Mod+Up"],
  "nav.end": ["Mod+End", "Mod+Down"],
  "cursor.left": ["Left"],
  "cursor.right": ["Right"],
  "cursor.up": ["Up"],
  "cursor.down": ["Down"],
  "cursor.pageUp": ["PageUp"],
  "cursor.pageDown": ["PageDown"],
  "cursor.home": ["Home", "Mod+Left"],
  "cursor.end": ["End", "Mod+Right"],
  "select.left": ["Shift+Left"],
  "select.right": ["Shift+Right"],
  "select.up": ["Shift+Up"],
  "select.down": ["Shift+Down"],
  "select.pageUp": ["Shift+PageUp"],
  "select.pageDown": ["Shift+PageDown"],
  "select.home": ["Shift+Home", "Shift+Mod+Left"],
  "select.end": ["Shift+End", "Shift+Mod+Right"],
  "select.start": ["Shift+Mod+Home", "Shift+Mod+Up"],
  "select.fileEnd": ["Shift+Mod+End", "Shift+Mod+Down"],
  "cursor.nibbleLeft": ["Alt+Left"],
  "cursor.nibbleRight": ["Alt+Right"],
  "cursor.switchColumn": ["Tab"],
  "find.open": ["Mod+F"],
  "find.next": ["Mod+G", "F3"],
  "find.prev": ["Shift+Mod+G", "Shift+F3"],
  "nav.nextChange": ["Alt+Down"],
  "nav.prevChange": ["Alt+Up"],
  "view.palette": ["Mod+K", "Shift+Mod+P"],
  "view.left": ["Mod+B"],
  "view.right": ["Alt+Mod+I"],
  "view.zoomIn": ["Mod+Plus"],
  "view.zoomOut": ["Mod+Minus"],
  "view.zoomReset": ["Mod+0"],
  "view.settings": ["Mod+,"],
  "marks.add": ["Mod+D"],
  "diff.next": ["F7"],
  "help.commands": ["F1"],
  "tools.calc": ["Alt+Mod+K"],
  "edit.clipHistory": ["Alt+Mod+V"],
  "file.print": ["Mod+P"],
  "search.inFiles": ["Shift+Mod+F"],
  "struct.expand": ["Shift+Alt+Right"],
  "struct.shrink": ["Shift+Alt+Left"],
  "diff.prev": ["Shift+F7"],
  "view.foldSelection": ["Alt+Mod+["],
  "view.unfoldAll": ["Alt+Mod+]"],
  "view.split2": ["Mod+\\"],
  "marks.addNote": ["Shift+Mod+D"],
  "marks.next": ["F2"],
  "marks.prev": ["Shift+F2"],
  "file.props": ["Alt+Mod+P"],
  // chordOf() emits modifiers in the order Ctrl, Mod, Alt, Shift.
  "macro.record": ["Mod+Shift+R"],
};

const PRESETS: Record<KeymapPreset, Keymap> = {
  hexa: BASE,
  hxd: {
    ...BASE,
    "nav.goto": ["Mod+G"],
    "find.next": ["F3"],
    "find.prev": ["Shift+F3"],
    "edit.pasteOverwrite": ["Mod+B"],
    "view.left": ["Alt+Mod+B"],
    "select.range": ["Mod+E"],
    "edit.copyAs": ["Shift+Mod+C"],
  },
  "010": {
    ...BASE,
    "nav.goto": ["Mod+G"],
    "find.next": ["F3"],
    "find.prev": ["Shift+F3"],
    "edit.copyHex": ["Shift+Mod+C"],
    "edit.copyAs": ["Alt+Mod+C"],
    "edit.pasteSpecial": ["Shift+Mod+V"],
    "select.range": ["Shift+Mod+A"],
  },
  vim: BASE,
  emacs: {
    ...BASE,
    "file.open": ["Ctrl+X Ctrl+F"],
    "file.save": ["Ctrl+X Ctrl+S"],
    "file.saveAs": ["Ctrl+X Ctrl+W"],
    "file.close": ["Ctrl+X K"],
    "edit.undo": ["Ctrl+/", "Ctrl+X U"],
    "edit.cut": ["Ctrl+W"],
    "edit.copy": ["Alt+W"],
    "edit.paste": ["Ctrl+Y"],
    "find.open": ["Ctrl+S"],
    "find.prev": ["Ctrl+R"],
    "nav.goto": ["Alt+G G"],
    "cursor.right": ["Right", "Ctrl+F"],
    "cursor.left": ["Left", "Ctrl+B"],
    "cursor.down": ["Down", "Ctrl+N"],
    "cursor.up": ["Up", "Ctrl+P"],
    "cursor.home": ["Home", "Ctrl+A"],
    "cursor.end": ["End", "Ctrl+E"],
    "cursor.pageDown": ["PageDown", "Ctrl+V"],
    "cursor.pageUp": ["PageUp", "Alt+V"],
    "nav.start": ["Alt+Shift+<", "Mod+Home"],
    "nav.end": ["Alt+Shift+>", "Mod+End"],
    "view.palette": ["Alt+X", "Mod+K"],
    "select.all": ["Ctrl+X H"],
  },
};

/** Effective bindings: preset plus the user's overrides. */
export function bindings(): Keymap {
  const st = settings();
  const preset = PRESETS[st.keymap] ?? BASE;
  const out: Keymap = { ...preset };
  if (!(st.keymap === "emacs") && !isMacPlatform()) {
    // Ctrl+Tab style bindings already use Mod on other platforms.
  }
  for (const [id, keys] of Object.entries(st.keyOverrides)) out[id] = keys;
  for (const [id, keys] of Object.entries(out)) out[id] = keys.map(normalizeBinding);
  return out;
}

function isMacPlatform() {
  return /Mac/.test(navigator.platform);
}

export function keysFor(id: string): string | undefined {
  return bindings()[id]?.[0];
}

/** Command for a chord sequence; `prefix` when more chords are expected. */
export function resolve(seq: string): { id?: string; prefix: boolean } {
  let prefix = false;
  for (const [id, keys] of Object.entries(bindings())) {
    for (const k of keys) {
      if (k === seq) return { id, prefix: false };
      if (k.startsWith(`${seq} `)) prefix = true;
    }
  }
  return { prefix };
}

/** Observers of executed commands (macro recording, SCR-03). */
export const commandHooks: { onRun?: (id: string) => void } = {};

export function runCommand(id: string): boolean {
  const c = commandById(id) ?? layoutCommands().find((l) => l.id === id);
  if (!c) return false;
  const t = activeTab();
  if (c.doc && !t) return false;
  if (c.write && t?.info.mode === "readOnly") {
    toast("Документ только для чтения — нажмите Insert, чтобы править", true);
    return true;
  }
  commandHooks.onRun?.(id);
  Promise.resolve(c.run()).catch(A.report);
  return true;
}
