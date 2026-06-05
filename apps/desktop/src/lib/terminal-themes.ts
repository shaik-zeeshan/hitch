// Curated terminal color themes for xterm.js. Palettes are sourced from the
// mbadolato/iTerm2-Color-Schemes repo (ghostty/ directory), transcribed to the
// xterm ITheme subset we apply. 10 dark + 10 light. The default ("Hitch")
// palette is built in elsewhere and selected via HITCH_THEME_ID.
import { writable, type Writable } from "svelte/store";

/** Hex colors for one terminal theme; shape mirrors xterm's ITheme subset we use. */
export interface TerminalThemeColors {
  background: string;
  foreground: string;
  cursor: string;
  cursorAccent: string;
  selectionBackground: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
}

export interface TerminalThemeDef {
  id: string; // kebab-case, e.g. "dracula", "tokyo-night-day"
  name: string; // display name, e.g. "Dracula"
  mode: "dark" | "light";
  colors: TerminalThemeColors;
}

/** Sentinel id meaning "use Hitch's built-in palette" (the default). */
export const HITCH_THEME_ID = "hitch";

export const TERMINAL_THEMES: TerminalThemeDef[] = [
  // ---- Dark ----
  {
    id: "dracula",
    name: "Dracula",
    mode: "dark",
    colors: {
      background: "#282a36",
      foreground: "#f8f8f2",
      cursor: "#f8f8f2",
      cursorAccent: "#282a36",
      selectionBackground: "#44475a",
      black: "#21222c",
      red: "#ff5555",
      green: "#50fa7b",
      yellow: "#f1fa8c",
      blue: "#bd93f9",
      magenta: "#ff79c6",
      cyan: "#8be9fd",
      white: "#f8f8f2",
      brightBlack: "#6272a4",
      brightRed: "#ff6e6e",
      brightGreen: "#69ff94",
      brightYellow: "#ffffa5",
      brightBlue: "#d6acff",
      brightMagenta: "#ff92df",
      brightCyan: "#a4ffff",
      brightWhite: "#ffffff",
    },
  },
  {
    id: "one-dark",
    name: "One Dark",
    mode: "dark",
    colors: {
      background: "#21252b",
      foreground: "#abb2bf",
      cursor: "#abb2bf",
      cursorAccent: "#21252b",
      selectionBackground: "#323844",
      black: "#21252b",
      red: "#e06c75",
      green: "#98c379",
      yellow: "#e5c07b",
      blue: "#61afef",
      magenta: "#c678dd",
      cyan: "#56b6c2",
      white: "#abb2bf",
      brightBlack: "#767676",
      brightRed: "#e06c75",
      brightGreen: "#98c379",
      brightYellow: "#e5c07b",
      brightBlue: "#61afef",
      brightMagenta: "#c678dd",
      brightCyan: "#56b6c2",
      brightWhite: "#abb2bf",
    },
  },
  {
    id: "gruvbox-dark",
    name: "Gruvbox Dark",
    mode: "dark",
    colors: {
      background: "#282828",
      foreground: "#ebdbb2",
      cursor: "#ebdbb2",
      cursorAccent: "#282828",
      selectionBackground: "#665c54",
      black: "#282828",
      red: "#cc241d",
      green: "#98971a",
      yellow: "#d79921",
      blue: "#458588",
      magenta: "#b16286",
      cyan: "#689d6a",
      white: "#a89984",
      brightBlack: "#928374",
      brightRed: "#fb4934",
      brightGreen: "#b8bb26",
      brightYellow: "#fabd2f",
      brightBlue: "#83a598",
      brightMagenta: "#d3869b",
      brightCyan: "#8ec07c",
      brightWhite: "#ebdbb2",
    },
  },
  {
    id: "solarized-dark",
    name: "Solarized Dark",
    mode: "dark",
    colors: {
      background: "#001e27",
      foreground: "#708284",
      cursor: "#708284",
      cursorAccent: "#001e27",
      selectionBackground: "#002831",
      black: "#002831",
      red: "#d11c24",
      green: "#738a05",
      yellow: "#a57706",
      blue: "#2176c7",
      magenta: "#c61c6f",
      cyan: "#259286",
      white: "#eae3cb",
      brightBlack: "#475b62",
      brightRed: "#bd3613",
      brightGreen: "#475b62",
      brightYellow: "#536870",
      brightBlue: "#708284",
      brightMagenta: "#5956ba",
      brightCyan: "#819090",
      brightWhite: "#fcf4dc",
    },
  },
  {
    id: "nord",
    name: "Nord",
    mode: "dark",
    colors: {
      background: "#2e3440",
      foreground: "#d8dee9",
      cursor: "#eceff4",
      cursorAccent: "#2e3440",
      selectionBackground: "#eceff4",
      black: "#3b4252",
      red: "#bf616a",
      green: "#a3be8c",
      yellow: "#ebcb8b",
      blue: "#81a1c1",
      magenta: "#b48ead",
      cyan: "#88c0d0",
      white: "#e5e9f0",
      brightBlack: "#596377",
      brightRed: "#bf616a",
      brightGreen: "#a3be8c",
      brightYellow: "#ebcb8b",
      brightBlue: "#81a1c1",
      brightMagenta: "#b48ead",
      brightCyan: "#8fbcbb",
      brightWhite: "#eceff4",
    },
  },
  {
    id: "tokyo-night",
    name: "Tokyo Night",
    mode: "dark",
    colors: {
      background: "#1a1b26",
      foreground: "#c0caf5",
      cursor: "#c0caf5",
      cursorAccent: "#1a1b26",
      selectionBackground: "#33467c",
      black: "#15161e",
      red: "#f7768e",
      green: "#9ece6a",
      yellow: "#e0af68",
      blue: "#7aa2f7",
      magenta: "#bb9af7",
      cyan: "#7dcfff",
      white: "#a9b1d6",
      brightBlack: "#414868",
      brightRed: "#f7768e",
      brightGreen: "#9ece6a",
      brightYellow: "#e0af68",
      brightBlue: "#7aa2f7",
      brightMagenta: "#bb9af7",
      brightCyan: "#7dcfff",
      brightWhite: "#c0caf5",
    },
  },
  {
    id: "catppuccin-mocha",
    name: "Catppuccin Mocha",
    mode: "dark",
    colors: {
      background: "#1e1e2e",
      foreground: "#cdd6f4",
      cursor: "#f5e0dc",
      cursorAccent: "#1e1e2e",
      selectionBackground: "#585b70",
      black: "#45475a",
      red: "#f38ba8",
      green: "#a6e3a1",
      yellow: "#f9e2af",
      blue: "#89b4fa",
      magenta: "#f5c2e7",
      cyan: "#94e2d5",
      white: "#a6adc8",
      brightBlack: "#585b70",
      brightRed: "#f37799",
      brightGreen: "#89d88b",
      brightYellow: "#ebd391",
      brightBlue: "#74a8fc",
      brightMagenta: "#f2aede",
      brightCyan: "#6bd7ca",
      brightWhite: "#bac2de",
    },
  },
  {
    id: "monokai",
    name: "Monokai",
    mode: "dark",
    colors: {
      background: "#272822",
      foreground: "#fdfff1",
      cursor: "#c0c1b5",
      cursorAccent: "#272822",
      selectionBackground: "#57584f",
      black: "#272822",
      red: "#f92672",
      green: "#a6e22e",
      yellow: "#e6db74",
      blue: "#fd971f",
      magenta: "#ae81ff",
      cyan: "#66d9ef",
      white: "#fdfff1",
      brightBlack: "#6e7066",
      brightRed: "#f92672",
      brightGreen: "#a6e22e",
      brightYellow: "#e6db74",
      brightBlue: "#fd971f",
      brightMagenta: "#ae81ff",
      brightCyan: "#66d9ef",
      brightWhite: "#fdfff1",
    },
  },
  {
    id: "github-dark",
    name: "GitHub Dark",
    mode: "dark",
    colors: {
      background: "#0d1117",
      foreground: "#e6edf3",
      cursor: "#2f81f7",
      cursorAccent: "#0d1117",
      selectionBackground: "#264f78",
      black: "#484f58",
      red: "#ff7b72",
      green: "#3fb950",
      yellow: "#d29922",
      blue: "#58a6ff",
      magenta: "#bc8cff",
      cyan: "#39c5cf",
      white: "#b1bac4",
      brightBlack: "#6e7681",
      brightRed: "#ffa198",
      brightGreen: "#56d364",
      brightYellow: "#e3b341",
      brightBlue: "#79c0ff",
      brightMagenta: "#d2a8ff",
      brightCyan: "#56d4dd",
      brightWhite: "#ffffff",
    },
  },
  {
    id: "ayu-dark",
    name: "Ayu Dark",
    mode: "dark",
    colors: {
      background: "#0b0e14",
      foreground: "#bfbdb6",
      cursor: "#e6b450",
      cursorAccent: "#0b0e14",
      selectionBackground: "#409fff",
      black: "#11151c",
      red: "#ea6c73",
      green: "#7fd962",
      yellow: "#f9af4f",
      blue: "#53bdfa",
      magenta: "#cda1fa",
      cyan: "#90e1c6",
      white: "#c7c7c7",
      brightBlack: "#686868",
      brightRed: "#f07178",
      brightGreen: "#aad94c",
      brightYellow: "#ffb454",
      brightBlue: "#59c2ff",
      brightMagenta: "#d2a6ff",
      brightCyan: "#95e6cb",
      brightWhite: "#ffffff",
    },
  },
  // ---- Light ----
  {
    id: "solarized-light",
    name: "Solarized Light",
    mode: "light",
    colors: {
      background: "#fdf6e3",
      foreground: "#657b83",
      cursor: "#657b83",
      cursorAccent: "#fdf6e3",
      selectionBackground: "#eee8d5",
      black: "#073642",
      red: "#dc322f",
      green: "#859900",
      yellow: "#b58900",
      blue: "#268bd2",
      magenta: "#d33682",
      cyan: "#2aa198",
      white: "#bbb5a2",
      brightBlack: "#002b36",
      brightRed: "#cb4b16",
      brightGreen: "#586e75",
      brightYellow: "#657b83",
      brightBlue: "#839496",
      brightMagenta: "#6c71c4",
      brightCyan: "#93a1a1",
      brightWhite: "#fdf6e3",
    },
  },
  {
    id: "github-light",
    name: "GitHub Light",
    mode: "light",
    colors: {
      background: "#ffffff",
      foreground: "#1f2328",
      cursor: "#0969da",
      cursorAccent: "#ffffff",
      selectionBackground: "#d7d4f0",
      black: "#24292f",
      red: "#cf222e",
      green: "#116329",
      yellow: "#4d2d00",
      blue: "#0969da",
      magenta: "#8250df",
      cyan: "#1b7c83",
      white: "#6e7781",
      brightBlack: "#57606a",
      brightRed: "#a40e26",
      brightGreen: "#1a7f37",
      brightYellow: "#633c01",
      brightBlue: "#218bff",
      brightMagenta: "#a475f9",
      brightCyan: "#3192aa",
      brightWhite: "#8c959f",
    },
  },
  {
    id: "one-light",
    name: "One Light",
    mode: "light",
    colors: {
      background: "#f9f9f9",
      foreground: "#2a2c33",
      cursor: "#bbbbbb",
      cursorAccent: "#f9f9f9",
      selectionBackground: "#ededed",
      black: "#000000",
      red: "#de3e35",
      green: "#3f953a",
      yellow: "#d2b67c",
      blue: "#2f5af3",
      magenta: "#950095",
      cyan: "#3f953a",
      white: "#bbbbbb",
      brightBlack: "#000000",
      brightRed: "#de3e35",
      brightGreen: "#3f953a",
      brightYellow: "#d2b67c",
      brightBlue: "#2f5af3",
      brightMagenta: "#a00095",
      brightCyan: "#3f953a",
      brightWhite: "#ffffff",
    },
  },
  {
    id: "gruvbox-light",
    name: "Gruvbox Light",
    mode: "light",
    colors: {
      background: "#fbf1c7",
      foreground: "#3c3836",
      cursor: "#3c3836",
      cursorAccent: "#fbf1c7",
      selectionBackground: "#d5c4a1",
      black: "#fbf1c7",
      red: "#cc241d",
      green: "#98971a",
      yellow: "#d79921",
      blue: "#458588",
      magenta: "#b16286",
      cyan: "#689d6a",
      white: "#7c6f64",
      brightBlack: "#928374",
      brightRed: "#9d0006",
      brightGreen: "#79740e",
      brightYellow: "#b57614",
      brightBlue: "#076678",
      brightMagenta: "#8f3f71",
      brightCyan: "#427b58",
      brightWhite: "#3c3836",
    },
  },
  {
    id: "catppuccin-latte",
    name: "Catppuccin Latte",
    mode: "light",
    colors: {
      background: "#eff1f5",
      foreground: "#4c4f69",
      cursor: "#dc8a78",
      cursorAccent: "#eff1f5",
      selectionBackground: "#acb0be",
      black: "#5c5f77",
      red: "#d20f39",
      green: "#40a02b",
      yellow: "#df8e1d",
      blue: "#1e66f5",
      magenta: "#ea76cb",
      cyan: "#179299",
      white: "#acb0be",
      brightBlack: "#6c6f85",
      brightRed: "#de293e",
      brightGreen: "#49af3d",
      brightYellow: "#eea02d",
      brightBlue: "#456eff",
      brightMagenta: "#fe85d8",
      brightCyan: "#2d9fa8",
      brightWhite: "#bcc0cc",
    },
  },
  {
    id: "ayu-light",
    name: "Ayu Light",
    mode: "light",
    colors: {
      background: "#f8f9fa",
      foreground: "#5c6166",
      cursor: "#ffaa33",
      cursorAccent: "#f8f9fa",
      selectionBackground: "#035bd6",
      black: "#000000",
      red: "#ea6c6d",
      green: "#6cbf43",
      yellow: "#eca944",
      blue: "#3199e1",
      magenta: "#9e75c7",
      cyan: "#46ba94",
      white: "#bababa",
      brightBlack: "#686868",
      brightRed: "#f07171",
      brightGreen: "#86b300",
      brightYellow: "#f2ae49",
      brightBlue: "#399ee6",
      brightMagenta: "#a37acc",
      brightCyan: "#4cbf99",
      brightWhite: "#d1d1d1",
    },
  },
  {
    id: "tokyo-night-day",
    name: "Tokyo Night Day",
    mode: "light",
    colors: {
      background: "#e1e2e7",
      foreground: "#3760bf",
      cursor: "#3760bf",
      cursorAccent: "#e1e2e7",
      selectionBackground: "#99a7df",
      black: "#e9e9ed",
      red: "#f52a65",
      green: "#587539",
      yellow: "#8c6c3e",
      blue: "#2e7de9",
      magenta: "#9854f1",
      cyan: "#007197",
      white: "#6172b0",
      brightBlack: "#a1a6c5",
      brightRed: "#f52a65",
      brightGreen: "#587539",
      brightYellow: "#8c6c3e",
      brightBlue: "#2e7de9",
      brightMagenta: "#9854f1",
      brightCyan: "#007197",
      brightWhite: "#3760bf",
    },
  },
  {
    id: "rose-pine-dawn",
    name: "Rosé Pine Dawn",
    mode: "light",
    colors: {
      background: "#faf4ed",
      foreground: "#575279",
      cursor: "#575279",
      cursorAccent: "#faf4ed",
      selectionBackground: "#dfdad9",
      black: "#f2e9e1",
      red: "#b4637a",
      green: "#286983",
      yellow: "#ea9d34",
      blue: "#56949f",
      magenta: "#907aa9",
      cyan: "#d7827e",
      white: "#575279",
      brightBlack: "#9893a5",
      brightRed: "#b4637a",
      brightGreen: "#286983",
      brightYellow: "#ea9d34",
      brightBlue: "#56949f",
      brightMagenta: "#907aa9",
      brightCyan: "#d7827e",
      brightWhite: "#575279",
    },
  },
  {
    id: "everforest-light",
    name: "Everforest Light",
    mode: "light",
    colors: {
      background: "#efebd4",
      foreground: "#5c6a72",
      cursor: "#f57d26",
      cursorAccent: "#efebd4",
      selectionBackground: "#eaedc8",
      black: "#7a8478",
      red: "#e67e80",
      green: "#9ab373",
      yellow: "#c1a266",
      blue: "#7fbbb3",
      magenta: "#d699b6",
      cyan: "#83c092",
      white: "#b2af9f",
      brightBlack: "#a6b0a0",
      brightRed: "#f85552",
      brightGreen: "#8da101",
      brightYellow: "#dfa000",
      brightBlue: "#3a94c5",
      brightMagenta: "#df69ba",
      brightCyan: "#35a77c",
      brightWhite: "#fffbef",
    },
  },
  {
    id: "night-owl-light",
    name: "Night Owl Light",
    mode: "light",
    colors: {
      background: "#ffffff",
      foreground: "#403f53",
      cursor: "#403f53",
      cursorAccent: "#ffffff",
      selectionBackground: "#f2f2f2",
      black: "#011627",
      red: "#d3423e",
      green: "#2aa298",
      yellow: "#daaa01",
      blue: "#4876d6",
      magenta: "#403f53",
      cyan: "#08916a",
      white: "#7a8181",
      brightBlack: "#7a8181",
      brightRed: "#f76e6e",
      brightGreen: "#49d0c5",
      brightYellow: "#dac26b",
      brightBlue: "#5ca7e4",
      brightMagenta: "#697098",
      brightCyan: "#00c990",
      brightWhite: "#989fb1",
    },
  },
];

export function getTerminalTheme(id: string): TerminalThemeDef | undefined {
  return TERMINAL_THEMES.find((theme) => theme.id === id);
}

/**
 * Inline style that overrides the panel's surface CSS vars so the chrome AROUND
 * the xterm grid — the panel inset, on-surface overlays (search box, "new
 * output" pill), the session-tab strip's active tab, and the diff view — follows
 * the SELECTED terminal theme instead of the app's built-in `--term-*` tokens.
 * Without it a custom theme paints its background on the xterm canvas while the
 * surrounding inset still shows the paper/dusk tint, leaving a mismatched seam.
 *
 * Returns "" for HITCH_THEME_ID (or any unknown/stale id) so the built-in vars
 * stay EXACTLY as before — the out-of-box surface, gradients, and seam hairline
 * are untouched. Callers pass the id already resolved for the active app mode.
 *
 * What we set, and why each is needed on this surface:
 *  - --term-bg / --term-bg2: the panel fill = the theme's flat background, so
 *    the inset reads as one continuous surface with the grid.
 *  - --term-fg: overlay text (search input, pill label) on the theme bg.
 *  - --term-dim: search placeholder; a fg→bg mix keeps it legibly quieter.
 *  - --term-line: the search box / seam hairline; a low-contrast bg/fg mix so it
 *    reads on whatever surface the theme uses (light or dark).
 */
export function terminalSurfaceOverride(id: string): string {
  if (id === HITCH_THEME_ID) return "";
  const def = getTerminalTheme(id);
  if (!def) return "";
  const { background: bg, foreground: fg } = def.colors;
  return (
    `--term-bg:${bg};--term-bg2:${bg};--term-fg:${fg};` +
    `--term-dim:color-mix(in oklab, ${fg} 60%, ${bg});` +
    `--term-line:color-mix(in oklab, ${fg} 18%, ${bg});`
  );
}

// localStorage-backed writable (best-effort reads/writes). Mirrors the
// `persisted()` helper in settings.ts; kept local so this module owns its keys.
function persisted(key: string, initial: string): Writable<string> {
  let start = initial;
  try {
    start = localStorage.getItem(key) ?? initial;
  } catch {
    // localStorage can be unavailable (private mode / denied); fall back to the
    // default and keep an in-memory store for the session.
  }
  const store = writable(start);
  store.subscribe((value) => {
    try {
      localStorage.setItem(key, value);
    } catch {
      // See above — writes are best-effort.
    }
  });
  return store;
}

// Per-mode terminal theme selections, persisted across sessions. Default is the
// built-in Hitch palette (HITCH_THEME_ID) for both modes.
export const terminalThemeDark: Writable<string> = persisted("hitch.termThemeDark", HITCH_THEME_ID);
export const terminalThemeLight: Writable<string> = persisted("hitch.termThemeLight", HITCH_THEME_ID);
