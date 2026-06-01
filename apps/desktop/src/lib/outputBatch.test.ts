import { describe, expect, it } from "vitest";
import { concatTails, createOutputBatcher } from "./outputBatch";

const bytes = (s: string): Uint8Array => new TextEncoder().encode(s);
const text = (b: Uint8Array): string => new TextDecoder().decode(b);

describe("concatTails", () => {
  it("concatenates tails in order, preserving byte order exactly", () => {
    expect(text(concatTails([bytes("ab"), bytes("cd"), bytes("e")]))).toBe(
      "abcde",
    );
  });

  it("preserves a multi-byte UTF-8 sequence split across two tails", () => {
    // "é" (U+00E9) is 0xC3 0xA9; split across the tail boundary it must rejoin.
    const merged = concatTails([
      new Uint8Array([0xc3]),
      new Uint8Array([0xa9]),
    ]);
    expect(text(merged)).toBe("é");
  });

  it("returns an empty array for no tails", () => {
    expect(concatTails([]).length).toBe(0);
  });
});

describe("createOutputBatcher", () => {
  // A synchronous fake scheduler so we drive the flush by hand instead of
  // depending on real requestAnimationFrame timing. `schedule` stashes the
  // flush; `tick()` runs it once (one frame).
  function harness() {
    const writes: string[] = [];
    let scheduled: (() => void) | null = null;
    let nextHandle = 1;
    const batcher = createOutputBatcher({
      schedule: (flush) => {
        scheduled = flush;
        return nextHandle++;
      },
      cancel: () => {
        scheduled = null;
      },
      emit: (chunk) => writes.push(text(chunk)),
    });
    const tick = () => {
      const run = scheduled;
      scheduled = null;
      run?.();
    };
    return { batcher, writes, tick, isScheduled: () => scheduled !== null };
  }

  it("flushes multiple tails pushed within one frame as ONE ordered write", () => {
    const { batcher, writes, tick } = harness();
    batcher.push(bytes("foo"));
    batcher.push(bytes("bar"));
    batcher.push(bytes("baz"));
    expect(writes).toEqual([]); // nothing written before the frame flush
    tick();
    expect(writes).toEqual(["foobarbaz"]); // exactly one concatenated write
  });

  it("schedules only one flush per frame regardless of push count", () => {
    const { batcher, tick, writes } = harness();
    batcher.push(bytes("a"));
    batcher.push(bytes("b"));
    tick();
    expect(writes).toEqual(["ab"]);
    // A fresh push after the flush schedules a new frame.
    batcher.push(bytes("c"));
    tick();
    expect(writes).toEqual(["ab", "c"]);
  });

  it("ignores empty tails and does not schedule a flush for them", () => {
    const { batcher, isScheduled, writes, tick } = harness();
    batcher.push(new Uint8Array(0));
    expect(isScheduled()).toBe(false);
    tick();
    expect(writes).toEqual([]);
  });

  it("reset() drops pending tails — a pre-reset tail is not written after reset", () => {
    const { batcher, writes, tick } = harness();
    batcher.push(bytes("stale"));
    batcher.reset();
    tick(); // a stale scheduled flush, if any, runs here
    expect(writes).toEqual([]); // dropped, never written
    // Batcher is still usable after reset.
    batcher.push(bytes("fresh"));
    tick();
    expect(writes).toEqual(["fresh"]);
  });
});
