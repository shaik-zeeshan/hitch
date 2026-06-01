// Per-animation-frame coalescing of PTY output writes, split out of
// Terminal.svelte so the batching logic is unit-testable WITHOUT a live xterm
// or real requestAnimationFrame timing.
//
// WHY batch: the daemon delivers PTY bytes as many small Uint8Array tails per
// frame (one per channel message). Calling term.write once per tail thrashes
// xterm's parser/render scheduling under bursty output. Instead we buffer the
// tails arriving within a frame and hand xterm a SINGLE concatenated write per
// frame. Because we concatenate the RAW bytes in arrival order, multi-byte
// UTF-8 sequences split across tails stay intact, and xterm's streaming decoder
// sees exactly the same byte stream it would have from per-tail writes.

// Concatenate tails IN ORDER into one Uint8Array, preserving byte order exactly.
// Pure and xterm-free so the ordering property is directly testable.
export function concatTails(tails: Uint8Array[]): Uint8Array {
  let total = 0;
  for (const t of tails) total += t.length;
  const out = new Uint8Array(total);
  let offset = 0;
  for (const t of tails) {
    out.set(t, offset);
    offset += t.length;
  }
  return out;
}

// Scheduler-agnostic batcher: the caller injects how a flush is scheduled
// (requestAnimationFrame in prod, a manual trigger in tests) and what a flushed
// chunk does (term.write + bump `written` in prod, a sink in tests). This keeps
// the flush/reset semantics drivable synchronously without real rAF timing.
export interface OutputBatcher {
  // Buffer a tail; schedules a flush if none is pending. Empty tails are ignored.
  push(tail: Uint8Array): void;
  // Concatenate + emit the buffered tails as ONE write, then clear the buffer
  // and the pending-flush marker. No-op when nothing is buffered.
  flush(): void;
  // Drop all buffered tails and cancel any pending flush (used on reset/destroy
  // so buffered-but-unflushed bytes are NOT written after a wipe or teardown).
  reset(): void;
}

// `schedule(flush)` is invoked when the first tail of a frame is buffered; it
// must arrange for `flush` (passed in) to run once later (e.g. via rAF) and
// return a cancel handle. `cancel(handle)` cancels a pending scheduled flush.
// `emit(chunk)` consumes one concatenated chunk (term.write in prod).
export function createOutputBatcher(opts: {
  schedule: (flush: () => void) => number;
  cancel: (handle: number) => void;
  emit: (chunk: Uint8Array) => void;
}): OutputBatcher {
  let pending: Uint8Array[] = [];
  let frame: number | null = null;

  const flush = (): void => {
    frame = null;
    if (pending.length === 0) return;
    const chunk = concatTails(pending);
    pending = [];
    opts.emit(chunk);
  };

  return {
    push(tail: Uint8Array): void {
      if (tail.length === 0) return;
      pending.push(tail);
      if (frame === null) frame = opts.schedule(flush);
    },
    flush,
    reset(): void {
      pending = [];
      if (frame !== null) {
        opts.cancel(frame);
        frame = null;
      }
    },
  };
}
