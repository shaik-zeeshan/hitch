import { afterEach, describe, expect, it, vi } from "vitest";
import { releaseWebgl, retainWebgl, touchWebgl } from "./webglBudget";

// MAX_WARM_CONTEXTS in webglBudget.ts. The module-level LRU is shared across
// tests, so each test fully releases the ids it retains in an afterEach.
const CAP = 6;

describe("webglBudget", () => {
  const retained: string[] = [];
  const retain = (id: string, dispose: () => void) => {
    retained.push(id);
    retainWebgl(id, dispose);
  };
  afterEach(() => {
    for (const id of retained) releaseWebgl(id);
    retained.length = 0;
  });

  it("keeps contexts warm up to the cap without disposing any", () => {
    const disposers = Array.from({ length: CAP }, () => vi.fn());
    disposers.forEach((d, i) => retain(`s${i}`, d));
    for (const d of disposers) expect(d).not.toHaveBeenCalled();
  });

  it("evicts the least-recently-active once the cap is exceeded", () => {
    const disposers = Array.from({ length: CAP + 1 }, () => vi.fn());
    disposers.forEach((d, i) => retain(`s${i}`, d));
    // s0 was the oldest → evicted (its disposer called); the rest stay warm.
    expect(disposers[0]).toHaveBeenCalledTimes(1);
    for (let i = 1; i <= CAP; i++) {
      expect(disposers[i]).not.toHaveBeenCalled();
    }
  });

  it("touchWebgl bumps recency so a touched id survives the next eviction", () => {
    const disposers = Array.from({ length: CAP }, () => vi.fn());
    disposers.forEach((d, i) => retain(`s${i}`, d));
    // s0 is currently the coldest; touching it makes s1 the new victim.
    touchWebgl("s0");
    const extra = vi.fn();
    retain("extra", extra);
    expect(disposers[0]).not.toHaveBeenCalled(); // saved by the touch
    expect(disposers[1]).toHaveBeenCalledTimes(1); // new coldest, evicted
  });

  it("releaseWebgl removes bookkeeping WITHOUT calling the disposer", () => {
    const dispose = vi.fn();
    retain("solo", dispose);
    releaseWebgl("solo");
    retained.length = 0; // already released; don't double-release in afterEach
    expect(dispose).not.toHaveBeenCalled();
  });

  it("re-retaining the same id refreshes its disposer and recency, not its count", () => {
    const disposers = Array.from({ length: CAP }, () => vi.fn());
    disposers.forEach((d, i) => retain(`s${i}`, d));
    // Re-retain s0 (now most-recent) — count unchanged, nothing evicted.
    const fresh = vi.fn();
    retainWebgl("s0", fresh);
    for (const d of disposers) expect(d).not.toHaveBeenCalled();
    // One more new id evicts s1 (the new coldest), not the refreshed s0.
    const extra = vi.fn();
    retain("extra", extra);
    expect(disposers[1]).toHaveBeenCalledTimes(1);
    expect(disposers[0]).not.toHaveBeenCalled();
    expect(fresh).not.toHaveBeenCalled();
  });
});
