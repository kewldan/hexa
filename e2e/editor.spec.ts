import { expect, type Page, test } from "@playwright/test";

const mod = process.platform === "darwin" ? "Meta" : "Control";

async function invoke<T>(page: Page, cmd: string, args: Record<string, unknown>): Promise<T> {
  return page.evaluate(
    // biome-ignore lint/suspicious/noExplicitAny: test access to the IPC mock
    ([c, a]) => (window as any).__TAURI_INTERNALS__.invoke(c, a),
    [cmd, args] as const,
  ) as Promise<T>;
}

async function bytesAt(page: Page, offset: number, len: number): Promise<string> {
  const b = await invoke<number[]>(page, "doc_read", { id: 1, offset, len });
  return b.map((x) => x.toString(16).padStart(2, "0")).join(" ");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => localStorage.clear());
  await page.goto("/");
  await page.getByRole("button", { name: "Открыть пример" }).click();
  await expect(page.locator("[data-hx-grid]")).toBeVisible();
  await page.locator("[data-hx-grid]").focus();
});

test("start screen leads to the editor", async ({ page }) => {
  await expect(page.locator(".tab.on")).toContainText("Untitled");
  await expect(page.locator(".sb")).toContainText("0x00000000");
  await expect(page.locator(".insp")).toContainText("int32");
});

test("hex typing edits by nibbles and undo restores", async ({ page }) => {
  const before = await bytesAt(page, 0, 2);
  await page.keyboard.type("dead");
  await expect.poll(() => bytesAt(page, 0, 2)).toBe("de ad");
  await expect(page.locator(".sb")).toContainText("0x00000002");
  await expect(page.locator(".tab.on")).toHaveClass(/mod/);
  await page.keyboard.press(`${mod}+z`);
  await expect.poll(() => bytesAt(page, 0, 2)).toBe(before);
});

test("keyboard navigation and selection", async ({ page }) => {
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowRight");
  await expect(page.locator(".sb")).toContainText("0x00000011");
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Shift+ArrowRight");
  await expect(page.locator(".sb")).toContainText("Выделено");
  await expect(page.locator(".sb")).toContainText("3 · 0x3");
});

test("goto accepts expressions", async ({ page }) => {
  await page.keyboard.press(`${mod}+l`);
  const input = page.getByRole("textbox", { name: "Смещение или выражение" });
  await input.fill("end-0x10");
  await expect(page.locator(".gprev")).toContainText("0x00000FF0");
  await input.press("Enter");
  await expect(page.locator(".sb")).toContainText("0x00000FF0");
});

test("find bar finds text and steps through matches", async ({ page }) => {
  await page.keyboard.press(`${mod}+f`);
  await page.getByRole("button", { name: "Текст" }).click();
  const input = page.getByLabel("Что искать");
  await input.fill("Hexa");
  await expect(page.locator(".fcount")).toContainText("1");
  await input.press("Enter");
  await expect(page.locator(".sb")).toContainText("Выделено");
  await input.press("Escape");
  await expect(page.locator(".findbar")).toHaveCount(0);
});

test("command palette runs commands and understands the wrong layout", async ({ page }) => {
  await page.keyboard.press(`${mod}+k`);
  const input = page.getByRole("textbox", { name: "Команда" });
  await input.fill("ntv");
  await expect(page.locator(".pi").first()).toContainText("тем");
  await input.fill("режим");
  await expect(page.locator(".pi.on")).toBeVisible();
  await input.fill("Режим: вставка");
  await input.press("Enter");
  await expect(page.locator(".sbi.mode")).toHaveText("INS");
});

test("insert mode inserts and delete removes", async ({ page }) => {
  await page.keyboard.press("Insert");
  await expect(page.locator(".sbi.mode")).toHaveText("INS");
  const len0 = (await invoke<{ len: number }>(page, "doc_info", { id: 1 })).len;
  await page.keyboard.type("00");
  await expect
    .poll(async () => (await invoke<{ len: number }>(page, "doc_info", { id: 1 })).len)
    .toBe(len0 + 1);
  await page.keyboard.press("Backspace");
  await expect
    .poll(async () => (await invoke<{ len: number }>(page, "doc_info", { id: 1 })).len)
    .toBe(len0);
});

test("panels can be moved to the other side", async ({ page }) => {
  const tab = page.locator('[data-side="left"] .side-tab[aria-label="Хэши"]');
  const target = page.locator('[data-side="right"]');
  await tab.hover();
  await page.mouse.down();
  const box = (await target.boundingBox())!;
  await page.mouse.move(box.x + 20, box.y + 10, { steps: 8 });
  await page.mouse.up();
  await expect(page.locator('[data-side="right"] .side-tab[aria-label="Хэши"]')).toBeVisible();
});

test("settings switch the theme and persist", async ({ page }) => {
  await page.keyboard.press(`${mod}+,`);
  await page.locator(".dlg").getByRole("combobox").first().selectOption("contrast");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "contrast");
  await page.keyboard.press("Escape");
  await page.waitForTimeout(600); // settings are saved with a debounce
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "contrast");
});

test("copy and paste a selection", async ({ page }) => {
  const info = () => invoke<{ len: number }>(page, "doc_info", { id: 1 });
  const len0 = (await info()).len;
  const src = await bytesAt(page, 0, 3);
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press(`${mod}+c`);
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Insert");
  await page.keyboard.press(`${mod}+v`);
  await expect.poll(async () => (await info()).len).toBe(len0 + 3);
  await expect.poll(() => bytesAt(page, 0x12, 3)).toBe(src);
});

test("transform panel applies a recipe to the selection", async ({ page }) => {
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Shift+ArrowRight");
  const before = await bytesAt(page, 0, 3);
  await page.locator('.side-tab[aria-label="Преобразования"]').click();
  await page.getByRole("combobox", { name: "Добавить шаг" }).selectOption("not");
  await expect(page.locator(".xf-hex")).toBeVisible();
  await page.getByRole("button", { name: "Заменить выделение" }).click();
  const inverted = before
    .split(" ")
    .map((h) => (~Number.parseInt(h, 16) & 0xff).toString(16).padStart(2, "0"))
    .join(" ");
  await expect.poll(() => bytesAt(page, 0, 3)).toBe(inverted);
});

test("two tabs are compared side by side", async ({ page }) => {
  await page.keyboard.press(`${mod}+n`);
  await page.getByRole("button", { name: "Создать" }).click();
  await expect(page.locator(".tab")).toHaveCount(2);
  await page.locator('.side-tab[aria-label="Сравнение"]').click();
  await page.getByRole("button", { name: "Сравнить…" }).click();
  await page.locator(".dlg .opt").first().click();
  await expect(page.locator(".diff-bar")).toContainText("1 вставлено");
  await expect(page.locator(".editor-col")).toHaveCount(2);
  await page.keyboard.press("F7");
  await expect(page.locator(".diff-bar")).toContainText("1/1");
});

test("analysis and strings panels show results", async ({ page }) => {
  await page.locator('.side-tab[aria-label="Анализ"]').click();
  await expect(page.locator(".anl-cap").first()).toContainText("Энтропия");
  await expect(page.locator(".kv")).toContainText("бит/байт");
  await page.locator('.side-tab[aria-label="Строки"]').click();
  await page.locator(".ph", { hasText: "Строки" }).getByRole("button", { name: "Найти" }).click();
  await expect(page.locator(".str-row").first()).toBeVisible();
});

test("disassembler panel lists instructions at the cursor", async ({ page }) => {
  await page.locator('.side-tab[aria-label="Код"]').click();
  await expect(page.locator(".dis-row").first()).toContainText(".byte");
  await page.getByRole("tab", { name: "Файл" }).click();
  await expect(page.locator(".empty")).toContainText("Не исполняемый файл");
});

test("calculator evaluates and help lists commands", async ({ page }) => {
  await page.keyboard.press(`${mod}+k`);
  await page.getByRole("textbox", { name: "Команда" }).fill("Калькулятор");
  await page.getByRole("textbox", { name: "Команда" }).press("Enter");
  await page.getByRole("textbox", { name: "Выражение" }).fill("0x10 * 2 + 0b1");
  await expect(page.locator(".calc-kv")).toContainText("0x21");
  await page.locator(".dlg-f").getByRole("button", { name: "Закрыть" }).click();
  await page.keyboard.press("F1");
  await page.getByRole("textbox", { name: "Найти команду" }).fill("калькулятор");
  await expect(page.locator(".help-row")).toHaveCount(1);
});

test("structure panel shows the template tree and follows the cursor", async ({ page }) => {
  await page.locator('.side-tab[aria-label="Структура"]').click();
  await page.getByRole("combobox", { name: "Применить шаблон" }).selectOption("png");
  await expect(page.locator(".st-row").first()).toContainText("png");
  // The tree opens down to the field under the cursor by itself.
  await expect(page.locator(".st-row", { hasText: "signature" })).toBeVisible();
  await expect(page.locator(".st-row.hit")).toContainText("signature");
  await page.locator(".st-row", { hasText: "signature" }).click();
  await expect(page.locator(".sb")).toContainText("Выделено");
  await page.getByRole("tab", { name: "Редактор" }).click();
  await page.getByRole("textbox", { name: "Исходный текст шаблона" }).fill("struct {");
  await expect(page.locator(".st-diags .err")).toContainText("expected");
});
