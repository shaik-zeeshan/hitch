# Draft generation runs outside Agent Sessions

Hitch will support non-interactive draft generation for commit messages, commit bodies, PR titles, and PR descriptions through a separate **Draft Generator**, not by broadening the **Agent** model. Agents remain known CLIs running in PTY Sessions with Agent State; Draft Generator runs are daemon-owned, headless, and do not produce Agent State.

## Considered Options

- **Separate Draft Generator** (chosen) — supports AI-assisted git text while preserving the Session/Agent boundary from ADR 0002.
- **Broaden Agent to include headless harness runs** — rejected because it makes Agent State ambiguous and conflicts with Hitch's terminal-native model.
- **Frontend-only prompt/copy helper** — rejected because Hitch needs first-class commit/PR draft actions and daemon-owned git context.

## Consequences

- Draft generation gets its own provider abstraction and IPC requests, separate from the Agent Registry and hook installation.
- The daemon composes git context and draft providers; `src-tauri` remains a thin IPC client.
- The desktop Settings UI selects provider/model and sends that selection on draft-generation IPC requests; Codex models come from `codex debug models`, while Claude uses documented aliases/latest-known IDs.
- A deterministic stub provider remains available for tests and fallback, while Claude/Codex run as headless CLIs outside Agent Sessions.

## Amendment (2026-06-07): Draft Instructions append, never replace

`DraftGenerationSettings` gains two optional fields — commit instructions and PR instructions — user-written guidance (style, conventions, language) edited in Settings → Drafts and stored app-global like the other draft settings. The daemon injects a non-empty value into the built-in prompt as an "Additional instructions from the user" block before the diff; the built-in prompt — including the strict return-only-JSON output contract the daemon parses — is **never user-replaceable**. Full template replacement was rejected: one missing JSON instruction silently breaks every draft, and it would require template validation plus a reset affordance for marginal power. Instructions can shape tone and conventions but cannot widen the Draft Generator's capabilities (providers still run read-only / tool-less). The stub provider ignores instructions — it stays deterministic for tests and fallback.

## Amendment (2026-06-04): user-configurable provider binary paths (protocol v18)

Protocol v18 adds `claude_path` / `codex_path` to `DraftGenerationSettings`, letting the user point the Draft Generator at the actual provider binaries. This is needed on Windows, where `claude` / `codex` are not reliably on the daemon service's PATH. The daemon defaults each provider to a bare command name (`claude` / `codex`, resolved via PATH, or the `HITCH_CLAUDE_PATH` / `HITCH_CODEX_PATH` env overrides); a path supplied in settings is trimmed and applied only when non-empty (a blank/whitespace value is ignored, so the default still wins). There is no pre-flight existence check: if the resolved binary cannot be spawned, generation fails with an actionable error ("… binary not found (`<path>`); install it or set HITCH_CLAUDE_PATH") rather than silently. This stays inside the Draft Generator boundary — these paths configure a headless Job, never an Agent Session, and produce no Agent State.
