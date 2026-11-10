import { expect, type Page, test } from "@playwright/test";

const mod = process.platform === "darwin" ? "Meta" : "Control";

async function runCommand(page: Page, title: string) {
  await page.keyboard.press(`${mod}+k`);
  const input = page.getByRole("textbox", { name: "Команда" });
  await input.fill(title);
  await expect(page.locator(".pi.on")).toContainText(title);
  await input.press("Enter");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => localStorage.clear());
  await page.goto("/");
  await page.getByRole("button", { name: "Открыть пример" }).click();
  await expect(page.locator("[data-hx-grid]")).toBeVisible();
  await page.locator("[data-hx-grid]").focus();
});

test("process dialog lists, filters and explains a denied open", async ({ page }) => {
  await runCommand(page, "Открыть процесс…");
  const dlg = page.getByRole("dialog", { name: "Открыть процесс" });
  await expect(dlg).toBeVisible();
  const list = dlg.getByRole("list", { name: "Процессы" });
  await expect(list).toContainText("Finder");
  await expect(list).toContainText("game.exe");
  await dlg.getByRole("textbox", { name: "Поиск процесса" }).fill("game");
  await expect(list).not.toContainText("Finder");
  await dlg.getByRole("combobox", { name: "Разрядность" }).selectOption("64");
  await expect(list).not.toContainText("game.exe");
  await dlg.getByRole("combobox", { name: "Разрядность" }).selectOption("");
  await list.getByRole("button", { name: /game\.exe/ }).click();
  await dlg.getByRole("button", { name: "Открыть", exact: true }).click();
  await expect(dlg).toContainText("macOS не разрешает читать память");
  await expect(dlg).toContainText("Запустите Hexa через sudo");
});

test("disk dialog and memory panel render against the mock", async ({ page }) => {
  await runCommand(page, "Открыть диск…");
  const dlg = page.getByRole("dialog", { name: "Открыть диск" });
  await expect(dlg).toContainText("SanDisk Ultra");
  await expect(dlg.getByRole("button", { name: "Открыть только для чтения" })).toBeDisabled();
  await dlg.getByRole("button", { name: /SanDisk Ultra/ }).click();
  await expect(dlg.getByRole("button", { name: "Открыть только для чтения" })).toBeEnabled();
  await page.keyboard.press("Escape");
  await expect(dlg).toBeHidden();

  await runCommand(page, "Панель памяти: регионы, сканер, указатели");
  await expect(page.getByRole("tab", { name: "Память" })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByText("Память процесса", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Открыть процесс…" }).click();
  await expect(page.getByRole("dialog", { name: "Открыть процесс" })).toBeVisible();
});
