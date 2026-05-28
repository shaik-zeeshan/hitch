<!--
  Route `/` — the workspace. The actual 3-pane shell (TopNav · LeftRail ·
  Center · RightRail) lives in +layout.svelte so it is mounted exactly once
  and survives navigation to peer routes like /settings; that's what keeps
  every live xterm + its PTY-aligned grid intact when the user pops over to
  settings and back. There is nothing route-specific to render here.
-->
