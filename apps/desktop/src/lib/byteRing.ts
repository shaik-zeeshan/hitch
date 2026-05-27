// A bounded byte ring for one session's working PTY scrollback (ADR 0007).
//
// The daemon stays authoritative for scrollback; this is only the GUI's
// repaint copy, so it holds at most `capacity` bytes and trims from the HEAD
// when output runs past the cap (mirroring the daemon's
// `DEFAULT_SCROLLBACK_CAPACITY = 1024 * 1024` ScrollbackBuffer).
//
// The key invariant — and the whole reason this is a module with tests — is
// that callers track a TOTAL-BYTES-SEEN offset, not a current-buffer length.
// `totalSeen` is monotonic and never decreases when the head is trimmed, so the
// append/paint math an xterm consumer does (`bytesSince(written)`) stays correct
// across head-trims: a trim drops the oldest retained bytes but never rewrites
// the offset coordinate space.

// Mirror the daemon's bounded scrollback capacity (1 MiB / session).
export const DEFAULT_RING_CAPACITY = 1024 * 1024;

export class ByteRing {
  // Retained bytes: the most recent `<= capacity` bytes ever appended.
  #buffer: Uint8Array;
  // Monotonic count of ALL bytes ever appended; never decreases on trim.
  #totalSeen = 0;

  constructor(private readonly capacity: number = DEFAULT_RING_CAPACITY) {
    this.#buffer = new Uint8Array(0);
  }

  /** Total bytes ever appended (the offset basis); monotonic across trims. */
  get totalSeen(): number {
    return this.#totalSeen;
  }

  /** Number of bytes currently retained (after any head-trims). */
  get length(): number {
    return this.#buffer.length;
  }

  /** Append `bytes`; if the retained buffer would exceed capacity, trim the head. */
  append(bytes: Uint8Array): void {
    if (bytes.length === 0) return;
    this.#totalSeen += bytes.length;

    // Common case: appending an already-oversized chunk — keep only its tail.
    if (bytes.length >= this.capacity) {
      this.#buffer = bytes.slice(bytes.length - this.capacity);
      return;
    }

    const combinedLength = this.#buffer.length + bytes.length;
    if (combinedLength <= this.capacity) {
      const next = new Uint8Array(combinedLength);
      next.set(this.#buffer, 0);
      next.set(bytes, this.#buffer.length);
      this.#buffer = next;
      return;
    }

    // Overflow: keep the latest `capacity` bytes, trimming the oldest head.
    const next = new Uint8Array(this.capacity);
    const keptFromOld = this.capacity - bytes.length;
    next.set(this.#buffer.subarray(this.#buffer.length - keptFromOld), 0);
    next.set(bytes, keptFromOld);
    this.#buffer = next;
  }

  /**
   * Bytes from `offset` up to `totalSeen` that are STILL retained.
   *
   * - `offset >= totalSeen` → empty (caller is already up to date).
   * - `offset` older than the oldest retained byte
   *   (`offset < totalSeen - length`) → all retained bytes (caller fell behind
   *   and must repaint from the ring's current head).
   * - otherwise → the exact tail since `offset`, with no loss and no overlap.
   */
  bytesSince(offset: number): Uint8Array {
    if (offset >= this.#totalSeen) return new Uint8Array(0);
    const oldestRetained = this.#totalSeen - this.#buffer.length;
    if (offset <= oldestRetained) {
      // Fell behind (or brand new): hand back everything we still hold.
      return this.#buffer.slice();
    }
    return this.#buffer.slice(offset - oldestRetained);
  }

  /** A copy of all currently retained bytes. */
  snapshot(): Uint8Array {
    return this.#buffer.slice();
  }
}
