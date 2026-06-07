import { describe, expect, it, vi } from "vitest";

import { focusWithoutScroll } from "./focusWithoutScroll";

// Regression for the "late-row click resets scroll / opens no tab" bug: a clipped
// row <button> focused on mousedown is scrolled into view by WebKit, stranding the
// click. The pointerdown handler must pre-focus the row with preventScroll so the
// browser's default focus is a no-op and the click survives. These tests pin the
// handler contract (the WebKit scroll behavior itself isn't reproducible in jsdom).
describe("focusWithoutScroll", () => {
  it("focuses the event's currentTarget with preventScroll", () => {
    const focus = vi.fn();
    const el = { focus } as unknown as HTMLElement;
    focusWithoutScroll({ currentTarget: el });
    expect(focus).toHaveBeenCalledTimes(1);
    expect(focus).toHaveBeenCalledWith({ preventScroll: true });
  });

  it("never scrolls: preventScroll is always set", () => {
    const focus = vi.fn();
    focusWithoutScroll({ currentTarget: { focus } as unknown as HTMLElement });
    const opts = focus.mock.calls[0][0];
    expect(opts).toBeDefined();
    expect(opts.preventScroll).toBe(true);
  });

  it("is a no-op when there is no currentTarget", () => {
    expect(() => focusWithoutScroll({ currentTarget: null })).not.toThrow();
  });
});
