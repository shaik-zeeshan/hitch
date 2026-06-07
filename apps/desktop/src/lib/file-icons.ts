// Per-file-type icons for the right-rail Changes list.
//
// We render the VS Code "Material Icon Theme" set (via the `vscode-material-icons`
// package — matejchalk's wrapper around the material-icon-theme assets) in full
// colour. Each changed file gets an instantly recognisable, multi-tone glyph.
//
// Deliberate aesthetic exception: the Paper Terminal shell is otherwise strictly
// monochrome, but at the 16px size of a changes row a one-ink mark is too hard to
// read at a glance, so we use full-colour glyphs here purely for recognition
// (user call, 2026-06-06). These SVGs carry their own hardcoded fills — we do NOT
// tint them; they render as <img> in RightRail, not inline {@html}.
//
// Why the package and not a hand-rolled extension→component map: its
// `getIconForFilePath(path)` already implements exactly the precedence we want —
//   1. exact file name        (package.json, tsconfig.json, Dockerfile, …)
//   2. lowercased file name
//   3. file *suffix* (everything after the first dot, so compound extensions like
//      `.test.ts` / `.d.ts` / `.config.js` resolve to their specific glyph first)
//   4. file extension (after the last dot)
//   5. `.html`/`.ts`/`.js` language fallbacks
//   6. a generic `"file"` glyph
// It keys off the basename only (it strips the directory itself) and never throws —
// it always returns a valid icon *name*, falling back to `"file"`.
//
// Asset strategy (Vite/Tauri): the package ships ~900 SVG files. We do NOT eagerly
// inline them as strings into the JS bundle. Instead `import.meta.glob(..., {
// query: '?url', eager: true })` lets Vite emit each referenced SVG as a hashed
// static asset and hands us a name→URL map of those (small) URLs. The icons live
// in node_modules; the emitted assets land in the build output and resolve under
// Tauri's custom protocol via relative paths.
import { getIconForFilePath } from "vscode-material-icons";

// name→URL map of every shipped material icon. `eager: true` + `query: '?url'`
// resolves at build time to hashed asset URLs (one short string each). The keys
// are module paths, so we re-key them by bare icon name (the file's basename).
// Bun may materialize this workspace dependency under apps/desktop/node_modules
// or hoist it to the workspace root, so include both fixed workspace layouts.
const iconUrlModules = import.meta.glob<string>(
  [
    "../../node_modules/vscode-material-icons/generated/icons/*.svg",
    "../../../../node_modules/vscode-material-icons/generated/icons/*.svg",
  ],
  { query: "?url", eager: true, import: "default", exhaustive: true },
);

const ICON_URLS: Record<string, string> = {};
for (const [modulePath, url] of Object.entries(iconUrlModules)) {
  const file = modulePath.slice(modulePath.lastIndexOf("/") + 1); // e.g. "rust.svg"
  const name = file.slice(0, -".svg".length); // e.g. "rust"
  ICON_URLS[name] = url;
}

// The library's universal fallback glyph; guaranteed to be in the set. Resolved
// once so callers always have a real asset URL even if a more specific icon name
// somehow has no file shipped for it.
const GENERIC_ICON_URL = ICON_URLS["file"] ?? "";

// Resolve a repo-relative (or absolute) path to the URL of its colourful material
// icon SVG. Delegates name resolution to the library (exact name → suffix →
// extension → language → `"file"`), then maps that name to its emitted asset URL.
// Never throws; always returns a usable URL string, falling back to the generic
// file glyph.
export function fileIconUrl(path: string): string {
  let name: string;
  try {
    name = getIconForFilePath(path);
  } catch {
    name = "file";
  }
  return ICON_URLS[name] ?? GENERIC_ICON_URL;
}

// Exposed for tests and callers that want the resolved icon *name* (not a URL),
// e.g. to assert precedence without depending on hashed asset filenames.
export function fileIconName(path: string): string {
  try {
    return getIconForFilePath(path);
  } catch {
    return "file";
  }
}
