// Loads the user-picked terminal font (settings.terminalFontFamily) into the
// webview as a WEB font. WKWebView's sandboxed web processes cannot see
// user-installed fonts at all — naming an installed family in font-family
// silently falls back, which is why Nerd Font icons rendered as tofu — so the
// desktop backend reads the picked family's face files off disk
// (list_terminal_font_faces / read_font_face) and we register the bytes via
// FontFace, which the sandbox never blocks. Registered under the SAME family
// name the picker stored, so the font-family stack from terminalFontStack()
// just works.
import { invoke } from "@tauri-apps/api/core";

type TerminalFontFace = { index: number; weight: number; italic: boolean };

// Families whose faces are registered in document.fonts (or were found to be
// unloadable — both are terminal states for a session, so cache either way and
// never re-fetch the multi-MB faces).
const settled = new Set<string>();
const pending = new Map<string, Promise<void>>();

// Resolves once `family`'s faces are usable (or determined unusable; loading
// is best-effort — the caller applies the font-family stack either way and
// fallback covers failure). Empty family = the built-in stack; nothing to load.
// TEMP DEBUG (remove): mirror progress into localStorage so it can be read
// from outside the webview.
function debugMark(value: string) {
  try {
    localStorage.setItem("hitch.debug.font", `${new Date().toISOString()} ${value}`);
  } catch {}
}

export function ensureTerminalFontLoaded(family: string): Promise<void> {
  const name = family.trim();
  if (!name || settled.has(name)) return Promise.resolve();
  const inFlight = pending.get(name);
  if (inFlight) return inFlight;
  debugMark(`loading ${name}`);
  const task = loadFaces(name)
    .then(() => {
      debugMark(`loaded ${name}; check=${document.fonts.check(`13px "${name}"`)}`);
    })
    .catch((err) => {
      debugMark(`FAILED ${name}: ${err instanceof Error ? err.message : String(err)}`);
      console.warn(`terminal font ${name} failed to load as a web font:`, err);
    })
    .finally(() => {
      settled.add(name);
      pending.delete(name);
    });
  pending.set(name, task);
  return task;
}

async function loadFaces(family: string): Promise<void> {
  const faces = await invoke<TerminalFontFace[]>("list_terminal_font_faces", { family });
  await Promise.all(
    faces.map(async (face) => {
      // The binary IPC response can arrive as ArrayBuffer or number[]
      // depending on the invoke path (same defensive wrap the official fs
      // plugin does). FontFace needs real BinaryData — anything else is
      // coerced to a string and parsed as a CSS `src:`, throwing SyntaxError.
      const data = await invoke<ArrayBuffer | number[]>("read_font_face", {
        family,
        index: face.index,
      });
      const bytes = data instanceof ArrayBuffer ? data : Uint8Array.from(data);
      const fontFace = new FontFace(family, bytes, {
        weight: String(face.weight),
        style: face.italic ? "italic" : "normal",
      });
      await fontFace.load();
      document.fonts.add(fontFace);
    }),
  );
}
