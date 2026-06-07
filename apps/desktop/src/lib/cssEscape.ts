// Minimal CSS.escape fallback shared by the rail row lists, which build attribute
// selectors from file paths (quotes, brackets) and git shas. CSS.escape exists in
// the webview; guard for the test/SSR environment just in case, where the global
// CSS object may be absent.
export function cssEscape(value: string): string {
  return typeof CSS !== "undefined" && CSS.escape ? CSS.escape(value) : value;
}
