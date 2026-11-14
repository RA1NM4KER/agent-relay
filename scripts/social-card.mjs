// Regenerate the static social preview from local brand assets; no external requests.
import { chromium } from 'playwright';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
const asset = name => new URL(`../assets/${name}`, import.meta.url);
const font = (await readFile(asset('fonts/geist-semibold.ttf'))).toString('base64');
const art = (await readFile(asset('relay-sculpture-v1.png'))).toString('base64');
const browser = await chromium.launch();
try {
  const page = await browser.newPage({ viewport: { width: 1200, height: 630 }, deviceScaleFactor: 1 });
  await page.setContent(`<!doctype html><html lang="en"><head><meta charset="utf-8"><style>
    @font-face {font-family:Geist;src:url(data:font/ttf;base64,${font})}*{box-sizing:border-box}body{margin:0;background:#0b0b0c;color:#f0eee9;font-family:Geist,sans-serif;width:1200px;height:630px;overflow:hidden;padding:48px 64px}
    .brand{font-size:24px;display:flex;align-items:center;gap:16px}.mark{display:grid;place-items:center;background:#e8b66f;color:#18130d;width:40px;height:40px;border-radius:8px}
    h1{font-size:60px;line-height:1;letter-spacing:-.05em;font-weight:600;margin:96px 0 24px;max-width:680px;position:relative;z-index:1}h1 span{color:#e8b66f}p{color:#aaa6a0;font-size:20px;line-height:28px;max-width:480px;margin:0;position:relative;z-index:1}img{position:absolute;right:0;top:104px;width:480px;height:490px;object-fit:contain}.url{position:absolute;left:64px;bottom:40px;color:#aaa6a0;font-size:16px}
    </style></head><body><div class="brand"><span class="mark">↳</span>Agent Relay</div><h1>Your agent hits a limit.<br><span>Your work carries on.</span></h1><p>Local handoffs for Claude Code and Codex.</p><img src="data:image/png;base64,${art}" alt=""><div class="url">ra1nm4ker.github.io/agent-relay</div></body></html>`);
  await page.evaluate(() => document.fonts.ready);
  await page.locator('img').evaluate(image => image.decode());
  await page.screenshot({path:fileURLToPath(asset('social-card.png'))});
  console.log('Generated assets/social-card.png (1200 × 630).');
} finally { await browser.close(); }
