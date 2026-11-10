import { expect, test } from "@playwright/test";

test("interface switches to English and back (UI-19)", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => localStorage.clear());
  await page.goto("/");
  await page.getByRole("button", { name: "Открыть пример" }).click();
  await expect(page.locator("[data-hx-grid]")).toBeVisible();
  const status = page.locator(".sb");
  await expect(status).toContainText("Смещение");

  await page.getByRole("button", { name: /^Настройки/ }).click();
  await page.getByRole("button", { name: "Доступность" }).click();
  await page.locator(".row-f", { hasText: "Язык интерфейса" }).locator("select").selectOption("en");

  await expect(page.locator("html")).toHaveAttribute("lang", "en");
  await expect(status).toContainText("Offset");
  await expect(status).not.toContainText("Смещение");
  await expect(page.getByRole("button", { name: "Inspector" }).first()).toBeVisible();
  await expect(page.locator(".row-f", { hasText: "Interface language" })).toBeVisible();

  await page
    .locator(".row-f", { hasText: "Interface language" })
    .locator("select")
    .selectOption("ru");
  await expect(page.locator("html")).toHaveAttribute("lang", "ru");
  await expect(status).toContainText("Смещение");
  await expect(page.locator(".row-f", { hasText: "Язык интерфейса" })).toBeVisible();
});
