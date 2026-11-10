import { expect, test } from "@playwright/test";

const mod = process.platform === "darwin" ? "Meta" : "Control";

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => localStorage.clear());
  await page.goto("/");
  await page.getByRole("button", { name: "Открыть пример" }).click();
  await expect(page.locator("[data-hx-grid]")).toBeVisible();
});

test("console evaluates an expression and keeps history", async ({ page }) => {
  await page.getByRole("tab", { name: "Консоль", exact: true }).click();
  const input = page.getByRole("textbox", { name: "Lua-консоль" });
  await input.fill("= 1 + 2 * 3");
  await input.press("Enter");
  const log = page.getByRole("log", { name: "Вывод консоли" });
  await expect(log).toContainText("› = 1 + 2 * 3");
  await expect(log.locator(".sc-l.value")).toHaveText("7");

  await input.fill('print("привет")');
  await input.press("Enter");
  await expect(log.locator(".sc-l.out")).toHaveText("привет");

  await input.press("ArrowUp");
  await expect(input).toHaveValue('print("привет")');

  await input.fill("hexa.fi");
  await input.press("Tab");
  await expect(input).toHaveValue("hexa.find_");
});

test("macro recording captures commands", async ({ page }) => {
  await page.locator("[data-hx-grid]").focus();
  await page.keyboard.press(`Shift+${mod}+r`);
  await expect(page.getByText("Запись макроса…", { exact: false })).toBeVisible();
  await page.keyboard.press(`${mod}+a`);
  await page.keyboard.press(`Shift+${mod}+r`);
  await expect(page.getByText("Макрос записан: 1", { exact: false })).toBeVisible();
  await page.getByRole("tab", { name: "Консоль", exact: true }).click();
  await page.getByRole("tab", { name: "Макросы" }).click();
  await expect(page.locator(".sc-steps")).toContainText("select.all");
});

test("find bar searches with a Lua predicate", async ({ page }) => {
  await page.locator("[data-hx-grid]").focus();
  await page.keyboard.press(`${mod}+f`);
  await page.getByRole("button", { name: "Lua" }).click();
  const input = page.getByLabel("Что искать");
  await input.fill("function(off, b) return b:byte(1) == 0x89 end");
  await input.press("Enter");
  await expect(page.locator(".fcount")).not.toHaveText("");
  await expect(page.locator(".fcount")).not.toContainText("нет совпадений");
});
