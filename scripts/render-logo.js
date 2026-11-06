// Renders assets/brand/*.svg to PNG at 2x using the bundled Playwright Chromium.
const { chromium } = require('playwright');
const fs = require('fs'); const path = require('path');
(async () => {
  const dir = path.join(__dirname, '..', 'assets', 'brand');
  const browser = await chromium.launch();
  for (const f of fs.readdirSync(dir).filter(f => f.endsWith('.svg'))) {
    const svg = fs.readFileSync(path.join(dir, f), 'utf8');
    const [, w, h] = svg.match(/width="(\d+)" height="(\d+)"/).map(Number);
    const page = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: 2 });
    await page.setContent(`<html><body style="margin:0;background:#fff">${svg}</body></html>`);
    await page.screenshot({ path: path.join(dir, f.replace('.svg', '.png')), omitBackground: false });
    await page.close();
  }
  await browser.close();
})();
