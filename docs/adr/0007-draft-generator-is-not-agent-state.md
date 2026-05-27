# Draft generation runs outside Agent Sessions

Hitch will support non-interactive draft generation for commit messages, commit bodies, PR titles, and PR descriptions through a separate **Draft Generator**, not by broadening the **Agent** model. Agents remain known CLIs running in PTY Sessions with Agent State; Draft Generator runs are daemon-owned, headless, and do not produce Agent State.

## Considered Options

- **Separate Draft Generator** (chosen) — supports AI-assisted git text while preserving the Session/Agent boundary from ADR 0002.
- **Broaden Agent to include headless harness runs** — rejected because it makes Agent State ambiguous and conflicts with Hitch's terminal-native model.
- **Frontend-only prompt/copy helper** — rejected because Hitch needs first-class commit/PR draft actions and daemon-owned git context.

## Consequences

- Draft generation gets its own provider abstraction and IPC requests, separate from the Agent Registry and hook installation.
- The daemon composes git context and draft providers; `src-tauri` remains a thin IPC client.
- Initial implementation can ship with a deterministic stub provider before wiring a real headless CLI provider.
