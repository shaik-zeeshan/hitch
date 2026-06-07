// Pointerdown focus helper shared by the scrollable rail row lists (History
// commit rows and Changes file rows).
//
// WHY: a row <button> that is partially clipped at the scroll viewport edge —
// the common case for a row the user reaches by scrolling — is focused by WebKit
// on mousedown, which then scrolls it fully into view. That scroll shifts the row
// out from under the pointer between mousedown and mouseup, so WebKit synthesizes
// NO `click`: the row's tab/diff never opens and the list appears to jump back.
// Initially-visible (fully in-view) rows aren't focus-scrolled, so they work —
// which is exactly why the bug only ever reproduced on scrolled-to rows and was
// invisible to synthetic-MouseEvent tests (which never trigger real focus).
//
// FIX: focus the row ourselves with `preventScroll` on pointerdown — before the
// browser's default mousedown-focus. The native focus then sees the element is
// already active and does nothing, so no scroll happens and the click lands.
// Keyboard roving is unaffected: it focuses rows through its own scrollIntoView
// paths, where the scroll is intentional.
export function focusWithoutScroll(event: { currentTarget: EventTarget | null }): void {
  const el = event.currentTarget as (HTMLElement & { focus(opts?: FocusOptions): void }) | null;
  el?.focus?.({ preventScroll: true });
}
