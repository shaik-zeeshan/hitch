// Keep WebGL contexts warm on the most-recently-active terminals instead of
// churning a GPU context on every tab/worktree switch. Each live WebGL context
// counts against the browser's ~16-context cap, so we bound the warm set well
// under it and evict the least-recently-active beyond the cap.
//
// This is a MODULE-LEVEL coordinator: every Terminal.svelte instance shares it
// so the cap is enforced across ALL live terminals, not per-component. A
// terminal registers its disposer via retainWebgl when it attaches WebGL; the
// budget calls that disposer to evict the coldest terminal once the warm set
// overflows. touchWebgl bumps recency without changing membership; releaseWebgl
// drops bookkeeping on component teardown (the component already disposed its
// own addon, so the budget must NOT call the disposer again).
const MAX_WARM_CONTEXTS = 6;

// Session ids ordered by recency, MOST-RECENT LAST. The head is the eviction
// victim once length exceeds the cap.
const lru: string[] = [];
// Per-session disposer that releases that terminal's actual WebGL addon.
const disposers = new Map<string, () => void>();

// Move an id to the most-recent end, dropping any prior position. Returns
// nothing; pure list bookkeeping shared by retain/touch.
function bump(id: string): void {
  const at = lru.indexOf(id);
  if (at !== -1) lru.splice(at, 1);
  lru.push(id);
}

// Register (or refresh) a terminal's warm WebGL context. Stores its disposer,
// marks it most-recently-active, then evicts the coldest terminals while the
// warm set exceeds the cap — calling each victim's disposer so its real addon
// is released and its GPU context returned to the browser's pool.
export function retainWebgl(id: string, dispose: () => void): void {
  disposers.set(id, dispose);
  bump(id);
  while (lru.length > MAX_WARM_CONTEXTS) {
    const victim = lru.shift();
    if (victim === undefined) break;
    const victimDispose = disposers.get(victim);
    disposers.delete(victim);
    // Releasing the evicted terminal's GPU context drops it to the DOM renderer;
    // it re-warms next time it becomes active and calls retainWebgl again.
    victimDispose?.();
  }
}

// Mark an already-warm terminal as most-recently-active (e.g. on hide, to keep
// the freshly-used terminal at the warm end). No eviction: membership and count
// are unchanged, so nothing can overflow.
export function touchWebgl(id: string): void {
  if (disposers.has(id)) bump(id);
}

// Forget a terminal entirely WITHOUT disposing — used on component destroy
// AFTER the component has already disposed its own addon, so calling the stored
// disposer again would be a redundant double-dispose.
export function releaseWebgl(id: string): void {
  const at = lru.indexOf(id);
  if (at !== -1) lru.splice(at, 1);
  disposers.delete(id);
}
