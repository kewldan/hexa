import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { extractSource, missingKeys } from "../../scripts/i18n-extract";
import {
  createTranslator,
  flushDomTranslation,
  startDomTranslation,
  stopDomTranslation,
  translate,
} from ".";

const root = resolve(import.meta.dirname, "../..");

describe("catalog", () => {
  it("translates every Russian string of the interface", () => {
    const missing = missingKeys(root).map((m) => `${m.file}:${m.line} ${JSON.stringify(m.key)}`);
    // Add these to src/i18n/en.json (`bun scripts/i18n-extract.ts` lists them too).
    expect(missing).toEqual([]);
  }, 30_000);
});

describe("extraction", () => {
  it("finds literals, JSX text and patterns", () => {
    const keys = extractSource(
      [
        'const a = <div title="Привет &amp; пока">',
        "  Текст {x}",
        "  ещё</div>;",
        // biome-ignore lint/suspicious/noTemplateCurlyInString: source code under test
        "const b = `Скопировано ${fmtSize(sel.len)} из ${total}`;",
        'const c = "Закладка " + n;',
        "const d = /[а-я]/;",
        'const e = { "ключ": 1 };',
        "// i18n-ignore",
        'const f = "пропуск";',
        'const g = "latin";',
      ].join("\n"),
      "a.tsx",
    ).map((f) => f.key);
    expect(keys).toEqual([
      "Привет & пока",
      "Текст",
      "ещё",
      "Скопировано {len} из {total}",
      "Закладка {n}",
    ]);
  });
});

describe("translate", () => {
  const tr = createTranslator({
    Закладка: "Bookmark",
    "Скопировано {len}": "Copied {len}",
    "Скопировано {len} из {total}": "Copied {len} of {total}",
    "{name} изменён": "{name} changed",
    "{#a} из {#b}": "{#a} of {#b}",
    "на месте": "in place",
    "Сохранено {how}": "Saved {how}",
    Пусто: "",
    "Смещение 0x{cursor}{byte}": "Offset 0x{cursor}{byte}",
    ", байт {b}": ", byte {b}",
  });

  it("looks up exact strings", () => {
    expect(tr("Закладка")).toBe("Bookmark");
  });

  it("fills patterns, longest first", () => {
    expect(tr("Скопировано 4 KB")).toBe("Copied 4 KB");
    expect(tr("Скопировано 4 KB из 8 KB")).toBe("Copied 4 KB of 8 KB");
  });

  it("translates captured Russian and keeps unknown captures", () => {
    expect(tr("Сохранено на месте")).toBe("Saved in place");
    expect(tr("отчёт.bin изменён")).toBe("отчёт.bin changed");
  });

  it("splits adjacent placeholders where the pieces are known", () => {
    expect(tr("Смещение 0x1F, байт 5")).toBe("Offset 0x1F, byte 5");
    expect(tr("Смещение 0x1F")).toBe("Offset 0x1F");
  });

  it("numeric placeholders match numbers only", () => {
    expect(tr("3 из 1 024+…")).toBe("3 of 1 024+…");
    expect(tr("три из пяти")).toBe("три из пяти");
  });

  it("keeps surrounding whitespace and collapses inner runs", () => {
    expect(tr("  Закладка\n")).toBe("  Bookmark\n");
    expect(tr("Скопировано\n   4 KB")).toBe("Copied 4 KB");
  });

  it("passes through unknown and non-Russian text", () => {
    expect(tr("Hello")).toBe("Hello");
    expect(tr("Неизвестная строка")).toBe("Неизвестная строка");
    expect(tr("Пусто")).toBe("Пусто");
  });

  it("translates the pieces of joined lists", () => {
    expect(tr("Закладка · на месте")).toBe("Bookmark · in place");
    expect(tr("Закладка · что-то")).toBe("Закладка · что-то");
  });

  it("uses the shipped catalog", () => {
    expect(translate("Перейти к смещению")).toBe("Go to offset");
    expect(translate("Скопировано 4 KB")).toBe("Copied 4 KB");
  });
});

describe("DOM translation", () => {
  afterEach(() => {
    stopDomTranslation();
    document.body.innerHTML = "";
  });

  it("translates text and attributes, skips user data and restores Russian", () => {
    document.body.innerHTML = [
      '<button title="Закладка">Закладка</button>',
      '<input placeholder="Закладка" value="Закладка">',
      '<textarea aria-label="Закладка">Закладка</textarea>',
      '<span data-no-i18n="">Закладка</span>',
      "<select><option>Закладка</option></select>",
    ].join("");
    const button = document.querySelector("button")!;
    const input = document.querySelector("input")!;
    startDomTranslation();
    expect(button.textContent).toBe("Bookmark");
    expect(button.title).toBe("Bookmark");
    expect(input.placeholder).toBe("Bookmark");
    expect(input.value).toBe("Закладка");
    expect(document.querySelector("textarea")!.value).toBe("Закладка");
    expect(document.querySelector("textarea")!.getAttribute("aria-label")).toBe("Bookmark");
    expect(document.querySelector("span")!.textContent).toBe("Закладка");
    expect(document.querySelector("option")!.textContent).toBe("Bookmark");

    // Text written later (as React does) is translated on the next flush.
    button.firstChild!.nodeValue = "Перейти к смещению";
    const added = document.createElement("div");
    added.innerHTML = '<b aria-label="Закладка">Скопировано 4 KB</b>';
    document.body.append(added);
    button.setAttribute("title", "Перейти к смещению");
    flushDomTranslation();
    expect(button.textContent).toBe("Go to offset");
    expect(button.title).toBe("Go to offset");
    expect(added.textContent).toBe("Copied 4 KB");
    expect(added.querySelector("b")!.getAttribute("aria-label")).toBe("Bookmark");

    stopDomTranslation();
    expect(button.textContent).toBe("Перейти к смещению");
    expect(button.title).toBe("Перейти к смещению");
    expect(input.placeholder).toBe("Закладка");
    expect(added.textContent).toBe("Скопировано 4 KB");
    expect(document.querySelector("option")!.textContent).toBe("Закладка");
  });
});
