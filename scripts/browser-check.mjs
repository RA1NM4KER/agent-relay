import { chromium } from "playwright";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, extname, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import assert from "node:assert/strict";
const require = createRequire(import.meta.url);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const mime = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".json": "application/json",
  ".svg": "image/svg+xml",
};
const server = createServer(async (req, res) => {
  try {
    const pathname = decodeURIComponent(
      new URL(req.url, "http://localhost").pathname,
    );
    assert(pathname.startsWith("/agent-relay/"));
    let path = resolve(
      root,
      pathname.slice("/agent-relay/".length) || "index.html",
    );
    assert(path.startsWith(root + "/"));
    const body = await readFile(path);
    res.writeHead(200, {
      "Content-Type": mime[extname(path)] || "application/octet-stream",
    });
    res.end(body);
  } catch (_) {
    res.writeHead(404);
    res.end("Not found");
  }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const base = `http://127.0.0.1:${server.address().port}/agent-relay/`;
const browser = await chromium.launch();
const artifacts = resolve(root, "artifacts");
await mkdir(artifacts, { recursive: true });
const pages = [
  "index.html",
  ...[
    "getting-started",
    "workflows",
    "commands",
    "profiles",
    "handoffs",
    "integrations",
    "troubleshooting",
    "advanced",
  ].map((s) => `docs/${s}.html`),
];
const errors = [];
try {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 1000 },
    permissions: ["clipboard-read", "clipboard-write"],
  });
  const page = await context.newPage();
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("response", (response) => {
    if (response.status() >= 400)
      errors.push(`${response.status()} ${response.url()}`);
  });
  for (const width of [1440, 768, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    for (const path of pages) {
      await page.goto(base + path);
      assert(
        (await page.locator("h1").count()) === 1,
        `${path}: expected one h1`,
      );
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth > innerWidth,
      );
      assert(!overflow, `${path}: horizontal overflow at ${width}px`);
      const gradients = await page.evaluate(
        () =>
          [...document.querySelectorAll("*")].filter((el) =>
            getComputedStyle(el).backgroundImage.includes("gradient("),
          ).length,
      );
      assert(gradients === 0, `${path}: unexpected gradient`);
      if (width === 1440 || width === 390) {
        await page.addScriptTag({
          path: require.resolve("axe-core/axe.min.js"),
        });
        for (const theme of ["dark", "light"]) {
          await page.evaluate((theme) => {
            document.documentElement.dataset.theme = theme;
          }, theme);
          const audit = await page.evaluate(() =>
            axe.run(document, {
              runOnly: {
                type: "tag",
                values: ["wcag2a", "wcag2aa", "wcag21aa"],
              },
            }),
          );
          const violations = audit.violations.map((v) => ({
            rule: v.id,
            nodes: v.nodes.map((n) => n.target),
          }));
          assert.deepEqual(
            violations,
            [],
            `${path}, ${width}px, ${theme}: accessibility violations`,
          );
        }
      }
    }
    console.log(
      `All pages: ${width}px layout${width === 1440 || width === 390 ? ", dark/light WCAG checks" : ""} passed.`,
    );
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto(base + "index.html");
  await page.locator(".copy-button").click();
  assert.equal(
    await page.evaluate(() => navigator.clipboard.readText()),
    "brew install RA1NM4KER/tap/agent-relay",
  );
  assert.equal(
    await page.locator("#copy-status").textContent(),
    "Command copied to clipboard.",
  );
  await page.getByRole("button", { name: "Switch to light theme" }).click();
  await page.reload();
  assert.equal(await page.locator("html").getAttribute("data-theme"), "light");
  await page.getByRole("button", { name: "Switch to dark theme" }).click();
  await page.goto(base + "docs/commands.html");
  await page.keyboard.press("/");
  assert(
    await page
      .locator("#docs-search")
      .evaluate((el) => el === document.activeElement),
  );
  await page.locator("#docs-search").fill("relay resume");
  await page.locator("#search-results a").first().waitFor();
  assert(
    (await page.locator("#search-results").textContent()).includes(
      "relay resume",
    ),
  );
  await page
    .locator("#search-results a")
    .filter({ has: page.locator("strong", { hasText: /^relay resume$/ }) })
    .first()
    .click();
  assert(page.url().endsWith("commands.html#resume"));
  assert(await page.locator("#search-results").isHidden());
  await page.locator("#docs-search").fill("zzzz-no-such-command");
  await page.waitForFunction(() =>
    document
      .getElementById("search-status")
      .textContent.startsWith("No results"),
  );
  await page.locator("#docs-search").fill("trust");
  await page.locator("#search-results a").first().waitFor();
  await page.keyboard.press("Escape");
  assert(await page.locator("#search-results").isHidden());
  // Clipboard failures must not falsely report success.
  await page.goto(base + "index.html");
  await page.evaluate(() => {
    navigator.clipboard.writeText = async () => {
      throw new Error("Denied");
    };
  });
  await page.locator(".copy-button").click();
  assert(
    (await page.locator("#copy-status").textContent()).startsWith(
      "Clipboard unavailable.",
    ),
  );
  assert.equal(
    await page.evaluate(() => window.getSelection().toString()),
    "brew install RA1NM4KER/tap/agent-relay",
  );
  // A search-index request failure leaves clear navigation guidance.
  await page.route("**/search-index.json", (route) => route.abort());
  await page.goto(base + "docs/getting-started.html");
  await page.locator("#docs-search").fill("resume");
  await page.waitForFunction(() =>
    document
      .getElementById("search-status")
      .textContent.startsWith("Search is unavailable."),
  );
  await page.unroute("**/search-index.json");
  await page.locator("#docs-search").fill("switch");
  await page.locator("#search-results a").first().waitFor();
  // Verify mobile navigation and a real destination, not just opening the menu.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(base + "docs/getting-started.html");
  await page.getByText("Browse documentation", { exact: true }).click();
  await page
    .locator(".mobile-doc-nav")
    .getByRole("link", { name: "Command reference", exact: true })
    .click();
  assert(page.url().endsWith("/docs/commands.html"));
  await page.screenshot({ path: resolve(artifacts, "docs-mobile.png") });
  await page.goto(base + "index.html");
  await page.screenshot({
    path: resolve(artifacts, "home-mobile.png"),
    fullPage: true,
  });
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.screenshot({
    path: resolve(artifacts, "home-desktop.png"),
    fullPage: true,
  });
  await page.getByRole("button", { name: "Switch to light theme" }).click();
  await page.screenshot({
    path: resolve(artifacts, "home-light.png"),
    fullPage: true,
  });
  await page.getByRole("button", { name: "Switch to dark theme" }).click();
  await page.goto(base + "docs/commands.html");
  await page.screenshot({ path: resolve(artifacts, "docs-desktop.png") });
  assert.deepEqual(
    errors,
    [],
    "Unexpected browser errors or missing resources",
  );
  const noJs = await browser.newContext({
    javaScriptEnabled: false,
    viewport: { width: 390, height: 844 },
  });
  const staticPage = await noJs.newPage();
  await staticPage.goto(base + "docs/getting-started.html");
  assert((await staticPage.locator("h1").textContent()) === "Get started");
  await staticPage.getByText("Browse documentation", { exact: true }).click();
  await staticPage
    .locator(".mobile-doc-nav")
    .getByRole("link", { name: "Everyday workflows", exact: true })
    .click();
  assert(staticPage.url().endsWith("workflows.html"));
  assert(await staticPage.locator(".copy-button").first().isHidden());
  await noJs.close();
  console.log(
    "Copy, denied clipboard, search, failed-search retry, theme persistence, keyboard, mobile navigation, no-JS content, and screenshots passed.",
  );
  await context.close();
} finally {
  await browser.close();
  await new Promise((resolve) => server.close(resolve));
}
