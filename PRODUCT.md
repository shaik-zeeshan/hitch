# Product

## Register

product

## Users

Working software developers who juggle several repositories and many parallel
branches at once, and who increasingly run AI coding agents (Claude Code, Codex)
alongside ordinary terminal work. They live in the terminal, are fluent with git
and keyboard shortcuts, and are skeptical of tools that get between them and the
CLI they already trust. Their context: a macOS desktop, often many things in
flight, needing to know at a glance which branch is dirty, which agent is
mid-run, and which one is blocked waiting on them.

The job to be done: manage Projects and their git Worktrees, host long-lived
terminal Sessions per worktree (shells, builds, or agent CLIs), surface git
status / commit / PR for the common flow, and report agent state, all without
forcing the developer out of the terminal. See `CONTEXT.md` for the precise
domain language (Project, Worktree, Session, Daemon, Agent, Agent State).

## Product Purpose

Hitch is a native macOS desktop app for running and supervising parallel
development work. It replaces a sprawl of terminal tabs and hand-rolled
`git worktree` commands with one place that shows the state of every worktree
and every agent run, and keeps that work alive across app quit via a long-lived
daemon that owns the PTYs.

Success looks like: a developer can see, in one glance, which of their N
worktrees needs attention; can jump into the live terminal for any of them
instantly; and never loses an agent run or a build to closing the window. Hitch
adds supervision and structure around the terminal, it never hijacks it.

## Brand Personality

Terminal-native and precise. Three words: **quiet, exact, trustworthy.**

The chrome is calm and recedes so the terminal and agent output are the star.
Monospace-forward where it carries meaning (paths, branches, state). Dense but
never cluttered: every pixel of UI earns its place by surfacing state the
developer would otherwise have to hunt for. The voice is plain and technical,
the confidence of a tool built by people who use it. No marketing gloss, no
hand-holding, no celebration animations.

## Anti-references

- **Generic SaaS dashboard.** No rounded cards everywhere, no gradient
  hero-metric panels, no Inter-on-white, no cookie-cutter Linear clone look.
- **Heavy IDE chrome.** Not VS Code: no dozens of competing toolbar icons, no
  nested panels, no busy multi-row status bars. Restraint over surface area.
- **Cluttered / loud.** No rainbow of accent colors, no badge soup, no
  decorative noise. One accent, used sparingly.
- **Cutesy / playful.** No illustrations, mascots, oversized rounded toy
  shapes, or friendly empty-state cartoons.

## Design Principles

1. **The terminal is the product; the shell serves it.** Hitch's UI exists to
   route attention and preserve work. When in doubt, give space to the PTY and
   shrink the chrome.
2. **Surface state, don't make them hunt.** The whole reason Hitch exists is the
   at-a-glance answer to "which worktree is dirty, which agent is blocked." That
   information must be legible from the tree without opening anything.
3. **State is never color alone.** Agent state and git state always pair a glyph
   or shape with any color, so the signal survives color blindness and grayscale
   screenshots.
4. **Keyboard-first, mouse-optional.** Every primary action is reachable and
   discoverable from the keyboard; shortcuts are surfaced, not hidden.
5. **Honest about the model.** The UI reflects Hitch's real domain (Projects,
   Worktrees, Sessions), not borrowed vocabulary. No fake "tasks," no chat UI an
   agent doesn't actually have.

## Accessibility & Inclusion

- Target WCAG 2.1 AA: text contrast >= 4.5:1 (>= 3:1 for large text and UI
  glyphs), visible focus rings on every interactive element, full keyboard
  navigation.
- Color-blind safe: agent state (running / needs-approval / completed / error)
  and git state (dirty / staged / ahead-behind) must be distinguishable without
  relying on hue, via glyph, shape, or position.
- Respect `prefers-reduced-motion`: motion is functional (state transitions,
  focus), never decorative, and degrades to instant when reduced motion is set.
