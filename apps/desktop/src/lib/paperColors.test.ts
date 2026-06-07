import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// The two canonical Paper Terminal `--paper-0` window-base colors (doc-design/
// colors.md) are encoded in THREE places with nothing else enforcing agreement:
//
//   1. app.html       — pre-paint inline script, hand-converted hex
//                        (#f9f6f1 light / #120e09 dusk)
//   2. src-tauri/src/lib.rs — PAPER_0_LIGHT / PAPER_0_DARK `Color(0x.., 0x.., 0x..)`
//   3. app.css        — canonical `--paper-0: oklch(...)` tokens
//                        (:root light + html[data-theme="dark"] override)
//
// If a future tweak moves the canonical app.css oklch() token without updating
// the hand-converted hex (app.html) and native window color (lib.rs), launch
// flashes the wrong colour for a frame — the exact bug this guards. This test
// converts the app.css oklch() values to sRGB itself and asserts all three
// sources agree per theme.

const here = dirname(fileURLToPath(import.meta.url));
const APP_HTML = resolve(here, "../app.html");
const APP_CSS = resolve(here, "../app.css");
const LIB_RS = resolve(here, "../../src-tauri/src/lib.rs");

type Rgb = readonly [number, number, number];

function rgbHex([r, g, b]: Rgb): string {
  return "#" + [r, g, b].map((c) => c.toString(16).padStart(2, "0")).join("");
}

// --- OKLCH -> sRGB (hand-rolled; no dependency) ----------------------------
// Standard Björn Ottosson pipeline: OKLCH -> OKLab -> linear sRGB -> gamma.
// L is the [0,1] lightness (CSS oklch() uses a 0%..100% percentage), C chroma,
// h hue in degrees. Matrices are the canonical OKLab constants.
function oklchToSrgb(L: number, C: number, hDeg: number): Rgb {
  const h = (hDeg * Math.PI) / 180;
  const a = C * Math.cos(h);
  const b = C * Math.sin(h);

  const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
  const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
  const s_ = L - 0.0894841775 * a - 1.291485548 * b;

  const l = l_ * l_ * l_;
  const m = m_ * m_ * m_;
  const s = s_ * s_ * s_;

  const lr = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
  const lg = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
  const lb = -0.0041960863 * l - 0.703418614 * m + 1.707614701 * s;

  const gamma = (c: number) => {
    const v = c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
    return Math.round(Math.max(0, Math.min(1, v)) * 255);
  };

  return [gamma(lr), gamma(lg), gamma(lb)] as const;
}

// --- Extractors ------------------------------------------------------------

function hexToRgb(hex: string): Rgb {
  const n = parseInt(hex.replace("#", ""), 16);
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff] as const;
}

// app.html: the pre-paint script sets `root.style.backgroundColor = dark ? "#..." : "#..."`.
// Anchor on that exact assignment so we read the values actually painted, not
// the hexes mentioned in the surrounding comment.
function readAppHtmlHexes(): { light: string; dark: string } {
  const src = readFileSync(APP_HTML, "utf8");
  const m = src.match(
    /root\.style\.backgroundColor\s*=\s*dark\s*\?\s*"(#[0-9a-fA-F]{6})"\s*:\s*"(#[0-9a-fA-F]{6})"/,
  );
  if (!m) {
    throw new Error(
      `paperColors: could not find the pre-paint backgroundColor assignment in ${APP_HTML}. ` +
        `Expected \`root.style.backgroundColor = dark ? "#..." : "#..."\`.`,
    );
  }
  return { dark: m[1].toLowerCase(), light: m[2].toLowerCase() };
}

// lib.rs: `const PAPER_0_LIGHT: Color = Color(0xf9, 0xf6, 0xf1, 0xff);`
function readLibRsRgb(name: string): Rgb {
  const src = readFileSync(LIB_RS, "utf8");
  const re = new RegExp(
    `${name}\\s*:\\s*Color\\s*=\\s*Color\\(\\s*0x([0-9a-fA-F]{2})\\s*,\\s*0x([0-9a-fA-F]{2})\\s*,\\s*0x([0-9a-fA-F]{2})\\s*,`,
  );
  const m = src.match(re);
  if (!m) {
    throw new Error(
      `paperColors: could not find \`${name}: Color = Color(0x.., 0x.., 0x.., ..)\` in ${LIB_RS}.`,
    );
  }
  return [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)] as const;
}

// app.css: extract `--paper-0: oklch(L% C H);` scoped to a given selector block.
// The light value lives under `:root {`, the dusk override under
// `html[data-theme="dark"] {`.
function readAppCssOklch(selector: string): Rgb {
  const src = readFileSync(APP_CSS, "utf8");
  const blockStart = src.indexOf(selector + " {");
  if (blockStart === -1) {
    throw new Error(`paperColors: could not find \`${selector} {\` block in ${APP_CSS}.`);
  }
  const blockEnd = src.indexOf("}", blockStart);
  const block = src.slice(blockStart, blockEnd);
  const m = block.match(
    /--paper-0:\s*oklch\(\s*([0-9.]+)%\s+([0-9.]+)\s+([0-9.]+)\s*\)/,
  );
  if (!m) {
    throw new Error(
      `paperColors: could not find \`--paper-0: oklch(...)\` inside the \`${selector}\` block in ${APP_CSS}.`,
    );
  }
  return oklchToSrgb(parseFloat(m[1]) / 100, parseFloat(m[2]), parseFloat(m[3]));
}

// --- Test ------------------------------------------------------------------

describe("--paper-0 window-base colors stay in sync across the three encodings", () => {
  const htmlHexes = readAppHtmlHexes();

  const themes = [
    {
      name: "light (:root)",
      cssSelector: ":root",
      rsConst: "PAPER_0_LIGHT",
      htmlHex: htmlHexes.light,
    },
    {
      name: "dark (html[data-theme=\"dark\"])",
      cssSelector: 'html[data-theme="dark"]',
      rsConst: "PAPER_0_DARK",
      htmlHex: htmlHexes.dark,
    },
  ] as const;

  for (const theme of themes) {
    it(`agree for the ${theme.name} theme (app.css oklch == app.html hex == lib.rs Color)`, () => {
      const css = readAppCssOklch(theme.cssSelector);
      const html = hexToRgb(theme.htmlHex);
      const rs = readLibRsRgb(theme.rsConst);

      // The three sources, located for the failure message:
      //   app.css   src/app.css                — oklch() canonical token
      //   app.html  src/app.html               — pre-paint hand-converted hex
      //   lib.rs    src-tauri/src/lib.rs       — native window Color()
      const detail =
        `\n  ${theme.name} --paper-0 encodings disagree:\n` +
        `    app.css  oklch -> ${rgbHex(css)}   (src/app.css, "${theme.cssSelector}" --paper-0 oklch token, the canonical source)\n` +
        `    app.html hex   -> ${rgbHex(html)}   (src/app.html, pre-paint script root.style.backgroundColor)\n` +
        `    lib.rs   Color -> ${rgbHex(rs)}   (src-tauri/src/lib.rs, ${theme.rsConst})\n` +
        `  Update all three to match the app.css oklch() token.`;

      // Hand-verified: the current oklch() tokens convert to EXACTLY the hand-
      // written hex (#f9f6f1 / #120e09), so we assert exact byte equality. If a
      // future oklch() edit lands a value that rounds within the OKLab matrices'
      // last-ULP wobble, relax the css<->others checks to a +/-1/255 tolerance
      // (toBeCloseTo-style per channel) — the documented hand-conversion
      // tolerance — instead of strict equality.
      expect(rgbHex(css), detail).toBe(rgbHex(html));
      expect(rgbHex(rs), detail).toBe(rgbHex(html));
      expect(rgbHex(rs), detail).toBe(rgbHex(css));
    });
  }
});
