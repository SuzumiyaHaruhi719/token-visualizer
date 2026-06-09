// capture-readme.mjs — drive the Token Visualizer dashboard in real Chrome and
// record the marquee GIFs + screenshots used by README.md.
//
// WHY Chrome (not the WebView2 app): WebView2 captures as a black frame in
// headless screen-grabs; the identical Vite/ECharts frontend renders perfectly
// in Chrome. We point puppeteer-core at the locally installed Chrome.
//
// SAFETY: we capture the dev server (`npm run dev`), where src/lib/api.ts forces
// MOCK mode (import.meta.env.DEV === true, no port hint). Every session row,
// chart, and number is synthetic demo data — no API tokens, no real prompts.
// The script also asserts the visible session text matches the known demo
// strings and aborts if anything unexpected (a possible real prompt) appears.
//
// Pipeline: puppeteer screenshot loop -> PNG frames -> ffmpeg palettegen +
// paletteuse -> optimized looping GIF. Raw frame folders are deleted after each
// clip (and are git-ignored anyway).
//
// Usage:
//   node scripts/capture-readme.mjs [--url http://127.0.0.1:5300] [--keep-frames]

import { spawnSync } from "node:child_process";
import { mkdirSync, rmSync, existsSync, writeFileSync, readdirSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, "..");
const ASSETS = join(ROOT, "assets");
const SHOTS = join(ROOT, "shots");
const FRAMES_ROOT = join(ROOT, ".capframes");

const args = process.argv.slice(2);
const argVal = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] ? args[i + 1] : fallback;
};
const URL = argVal("--url", "http://127.0.0.1:5300/");
const KEEP_FRAMES = args.includes("--keep-frames");

const CHROME =
  process.env.CHROME_PATH ||
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe";

// Capture geometry. DPR 1.5 makes the reels/text crisp; we downscale to <=960px
// wide in ffmpeg so the committed GIFs stay small.
const VIEW_W = 1280;
const VIEW_H = 800;
const DPR = 1.5;
const FPS = 14;
const OUT_W = 960; // final GIF width (downscaled from 1280*DPR)

// The exact demo strings our mock emits — used to *prove* no real prompt leaks.
const DEMO_MESSAGES = [
  "Add a by-source split bar to the dashboard",
  "Refactor the fan-curve solver and add tests",
  "Translate the onboarding chapter to English",
  "Run the browser QA pass on the landing page",
];

function sh(cmd, cmdArgs, opts = {}) {
  const r = spawnSync(cmd, cmdArgs, { stdio: "inherit", ...opts });
  if (r.status !== 0) {
    throw new Error(`${cmd} exited ${r.status}: ${cmdArgs.join(" ")}`);
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Record `durationMs` of `clip` by screenshotting an element (or full page) on
 *  a fixed-interval loop, then encode to a palette-optimized GIF. */
async function recordGif(page, { name, clip, durationMs, fps = FPS, outW = OUT_W }) {
  const frameDir = join(FRAMES_ROOT, name);
  rmSync(frameDir, { recursive: true, force: true });
  mkdirSync(frameDir, { recursive: true });

  const frameMs = 1000 / fps;
  const total = Math.round(durationMs / frameMs);
  const start = Date.now();
  for (let i = 0; i < total; i++) {
    const target = start + i * frameMs;
    const drift = target - Date.now();
    if (drift > 0) await sleep(drift);
    const file = join(frameDir, `f${String(i).padStart(4, "0")}.png`);
    if (clip) {
      await page.screenshot({ path: file, clip, captureBeyondViewport: true });
    } else {
      await page.screenshot({ path: file });
    }
  }

  const gifPath = join(ASSETS, `${name}.gif`);
  const palette = join(frameDir, "palette.png");
  const vf = `fps=${fps},scale=${outW}:-1:flags=lanczos`;
  // Two-pass palette for crisp, small GIFs (no gifski/IM available).
  sh("ffmpeg", [
    "-y", "-framerate", String(fps), "-i", join(frameDir, "f%04d.png"),
    "-vf", `${vf},palettegen=max_colors=160:stats_mode=diff`,
    palette,
  ]);
  sh("ffmpeg", [
    "-y", "-framerate", String(fps), "-i", join(frameDir, "f%04d.png"),
    "-i", palette,
    "-lavfi", `${vf} [x]; [x][1:v] paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle`,
    "-loop", "0",
    gifPath,
  ]);

  if (!KEEP_FRAMES) rmSync(frameDir, { recursive: true, force: true });
  const kb = (statSync(gifPath).size / 1024).toFixed(0);
  console.log(`  -> ${gifPath} (${kb} KB)`);
  return gifPath;
}

// NOTE on coordinates: puppeteer's screenshot `clip` is in PAGE (document)
// coordinates because we pass captureBeyondViewport. So we measure ABSOLUTE doc
// rects (getBoundingClientRect + scroll offset) and never scroll the page during
// capture — the strip below the fold is captured just as cleanly as the header.

function clampBox(box, label = "") {
  const x = Math.max(0, Math.round(box.x));
  const y = Math.max(0, Math.round(box.y));
  const width = Math.max(2, Math.min(Math.round(box.width), VIEW_W - x));
  const height = Math.max(2, Math.round(box.height));
  if (width < 2 || height < 2) {
    throw new Error(`degenerate clip for ${label}: ${JSON.stringify({ x, y, width, height })}`);
  }
  return { x, y, width, height };
}

/** Absolute-page bounding box of a selector (padded). */
async function boxOf(page, selector, pad = 0) {
  const box = await page.evaluate((sel) => {
    const node = document.querySelector(sel);
    if (!node) return null;
    const r = node.getBoundingClientRect();
    return { x: r.x + window.scrollX, y: r.y + window.scrollY, width: r.width, height: r.height };
  }, selector);
  if (!box) throw new Error(`selector not found: ${selector}`);
  return clampBox(
    { x: box.x - pad, y: box.y - pad, width: box.width + pad * 2, height: box.height + pad * 2 },
    selector,
  );
}

/** Union absolute-page bounding box across several selectors. */
async function unionBox(page, selectors, pad = 0) {
  const boxes = await page.evaluate((sels) =>
    sels.map((sel) => {
      const node = document.querySelector(sel);
      if (!node) return null;
      const r = node.getBoundingClientRect();
      return { x: r.x + window.scrollX, y: r.y + window.scrollY, width: r.width, height: r.height };
    }), selectors);
  const missing = selectors.filter((_, i) => !boxes[i]);
  if (missing.length) throw new Error(`selectors not found: ${missing.join(", ")}`);
  const x = Math.min(...boxes.map((b) => b.x));
  const y = Math.min(...boxes.map((b) => b.y));
  const right = Math.max(...boxes.map((b) => b.x + b.width));
  const bottom = Math.max(...boxes.map((b) => b.y + b.height));
  return clampBox(
    { x: x - pad, y: y - pad, width: right - x + pad * 2, height: bottom - y + pad * 2 },
    selectors.join("+"),
  );
}

async function main() {
  mkdirSync(ASSETS, { recursive: true });
  mkdirSync(SHOTS, { recursive: true });

  const browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: "new",
    defaultViewport: { width: VIEW_W, height: VIEW_H, deviceScaleFactor: DPR },
    args: [
      `--window-size=${VIEW_W},${VIEW_H}`,
      "--hide-scrollbars",
      "--force-color-profile=srgb",
      "--no-proxy-server", // mirror the app's 127.0.0.1 proxy-bypass
      "--disable-features=CalculateNativeWinOcclusion",
    ],
  });

  try {
    const page = await browser.newPage();
    page.on("pageerror", (e) => console.warn("[pageerror]", e.message));

    await page.goto(URL, { waitUntil: "networkidle2", timeout: 30000 });
    // Belt-and-suspenders: force mock even if env detection changes.
    await page.evaluate(() => {
      window.__CM_MOCK__ = true;
    });
    // Rebrand the visible title to the repo's product name for the captures
    // (capture-time only; not a code change).
    await page.evaluate(() => {
      const apply = () => {
        const brand = document.querySelector(".brand");
        if (brand) {
          brand.innerHTML = '<span class="hex">⬡</span> Token Visualizer';
          return true;
        }
        return false;
      };
      if (!apply()) {
        const obs = new MutationObserver(() => {
          if (apply()) obs.disconnect();
        });
        obs.observe(document.body, { childList: true, subtree: true });
      }
      document.title = "Token Visualizer";
    });

    await page.waitForSelector("#kpi-tokens", { timeout: 15000 });
    await page.waitForSelector(".cs-row", { timeout: 15000 });
    await sleep(1500); // let first SSE burst + tweens settle

    // --- SAFETY ASSERTION: every session row text must be a known demo string.
    const rowTexts = await page.$$eval(".cs-row .cs-msg", (els) =>
      els.map((e) => (e.textContent || "").trim()),
    );
    console.log("Session rows:", JSON.stringify(rowTexts));
    const allowed = new Set([...DEMO_MESSAGES, "—"]);
    const bad = rowTexts.filter((t) => !allowed.has(t));
    if (rowTexts.length === 0) throw new Error("no session rows rendered");
    if (bad.length > 0) {
      throw new Error(
        `ABORT: non-demo session text detected (possible real data): ${JSON.stringify(bad)}`,
      );
    }
    // Also assert no obvious secret patterns anywhere on the page.
    const bodyText = await page.evaluate(() => document.body.innerText);
    if (/sk-[A-Za-z0-9]{12,}|api[_-]?key|bearer\s+[A-Za-z0-9._-]{12,}/i.test(bodyText)) {
      throw new Error("ABORT: secret-like pattern detected in page text");
    }
    console.log("Safety check passed: only demo data present.\n");

    // Clips are absolute page coordinates (captureBeyondViewport), so we never
    // scroll. Keep the page at top for stable layout.
    await page.evaluate(() => window.scrollTo(0, 0));

    // ---------------------------------------------------------------- screenshots
    console.log("Screenshots...");
    await page.screenshot({ path: join(SHOTS, "dashboard-full.png"), fullPage: true });
    await page.screenshot({ path: join(SHOTS, "dashboard-top.png") });
    {
      const b = await unionBox(page, ["#chart-donut", "#chart-projects"], 14);
      await page.screenshot({ path: join(SHOTS, "charts.png"), clip: b, captureBeyondViewport: true });
    }
    {
      const b = await boxOf(page, "#bysource", 12);
      await page.screenshot({ path: join(SHOTS, "by-source.png"), clip: b, captureBeyondViewport: true });
    }
    {
      const b = await boxOf(page, "#chart-timeseries", 14);
      await page.screenshot({ path: join(SHOTS, "over-time.png"), clip: b, captureBeyondViewport: true });
    }
    {
      const b = await boxOf(page, "#current-strip", 12);
      await page.screenshot({ path: join(SHOTS, "sessions.png"), clip: b, captureBeyondViewport: true });
    }
    {
      const b = await boxOf(page, "#limits-body", 12);
      await page.screenshot({ path: join(SHOTS, "limits.png"), clip: b, captureBeyondViewport: true });
    }

    // ---------------------------------------------------------------- 1) HERO
    // Top of dashboard: brand + range tabs + KPIs + by-source + odometer rolling.
    console.log("GIF: hero (odometer + KPIs)...");
    const heroBox = await unionBox(page, [".topbar", "#token-ticker"], 0);
    heroBox.x = 0;
    heroBox.width = VIEW_W;
    heroBox.y = 0;
    await recordGif(page, { name: "hero", clip: heroBox, durationMs: 5200 });

    // ---------------------------------------------------------------- 2) ODOMETER
    console.log("GIF: odometer (token reels)...");
    const odoBox = await boxOf(page, "#token-ticker", 0);
    await recordGif(page, { name: "odometer", clip: odoBox, durationMs: 4600 });

    // ---------------------------------------------------------------- 3) RANGE SWITCH
    // KPIs + by-source + odometer + over-time chart all morph together.
    console.log("GIF: range switch (Today -> 7d -> 30d -> All)...");
    const switchBox = await unionBox(page, [".kpis", "#chart-timeseries"], 6);
    switchBox.x = 0;
    switchBox.width = VIEW_W;
    const clickRanges = (async () => {
      const order = ["7d", "30d", "all", "today"];
      await sleep(900);
      for (const key of order) {
        await page.click(`.range-tab[data-range="${key}"]`).catch(() => {});
        await sleep(1300);
      }
    })();
    const rangeRec = recordGif(page, {
      name: "range-switch",
      clip: switchBox,
      durationMs: 900 + 4 * 1300 + 400,
    });
    await Promise.all([clickRanges, rangeRec]);
    await page.click(`.range-tab[data-range="today"]`).catch(() => {});
    await sleep(800);

    // ---------------------------------------------------------------- 4) SESSIONS
    console.log("GIF: live sessions strip...");
    const sessBox = await boxOf(page, "#current-strip", 12);
    await recordGif(page, { name: "sessions", clip: sessBox, durationMs: 6800 });

    // ---------------------------------------------------------------- 5) CHARTS
    console.log("GIF: charts (by-model donut + top projects morphing)...");
    const chartsBox = await unionBox(page, ["#chart-donut", "#chart-projects"], 10);
    const chartsClicks = (async () => {
      await sleep(700);
      await page.click(`.range-tab[data-range="7d"]`).catch(() => {});
      await sleep(1500);
      await page.click(`.range-tab[data-range="30d"]`).catch(() => {});
      await sleep(1500);
      await page.click(`.range-tab[data-range="today"]`).catch(() => {});
      await sleep(900);
    })();
    const chartsRec = recordGif(page, {
      name: "charts",
      clip: chartsBox,
      durationMs: 700 + 1500 * 2 + 900 + 300,
    });
    await Promise.all([chartsClicks, chartsRec]);

    console.log("\nAll captures complete.");
  } finally {
    await browser.close();
    if (!KEEP_FRAMES) rmSync(FRAMES_ROOT, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
