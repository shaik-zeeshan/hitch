import { describe, expect, it } from "vitest";
import { ByteRing } from "./byteRing";

const bytes = (s: string): Uint8Array => new TextEncoder().encode(s);
const text = (b: Uint8Array): string => new TextDecoder().decode(b);

describe("ByteRing", () => {
  it("appends within capacity and reports length + totalSeen", () => {
    const ring = new ByteRing(16);
    ring.append(bytes("abc"));
    ring.append(bytes("def"));
    expect(text(ring.snapshot())).toBe("abcdef");
    expect(ring.length).toBe(6);
    expect(ring.totalSeen).toBe(6);
  });

  it("ignores empty appends", () => {
    const ring = new ByteRing(16);
    ring.append(bytes("ab"));
    ring.append(new Uint8Array(0));
    expect(text(ring.snapshot())).toBe("ab");
    expect(ring.totalSeen).toBe(2);
  });

  it("trims the head past capacity, keeping the latest bytes (mirrors scrollback_keeps_latest_bytes)", () => {
    const ring = new ByteRing(5);
    ring.append(bytes("abc"));
    ring.append(bytes("def"));
    expect(text(ring.snapshot())).toBe("bcdef");
    expect(ring.length).toBe(5);
    // totalSeen counts everything ever appended, not the retained length.
    expect(ring.totalSeen).toBe(6);
  });

  it("keeps only the tail when a single append exceeds capacity", () => {
    const ring = new ByteRing(4);
    ring.append(bytes("abcdefgh"));
    expect(text(ring.snapshot())).toBe("efgh");
    expect(ring.length).toBe(4);
    expect(ring.totalSeen).toBe(8);
  });

  it("bytesSince after a head-trim yields the correct tail with no duplication or loss for an up-to-date offset", () => {
    const ring = new ByteRing(5);
    ring.append(bytes("abc"));
    // A consumer caught up at offset 3 (totalSeen so far).
    const caughtUp = ring.totalSeen;
    expect(caughtUp).toBe(3);

    // More output arrives and forces a head-trim (retained becomes "bcdef").
    ring.append(bytes("def"));

    // The consumer asks for what it has not yet seen since offset 3.
    const tail = ring.bytesSince(caughtUp);
    // Bytes 3.. are "def" — still fully retained, no overlap with "abc".
    expect(text(tail)).toBe("def");
  });

  it("bytesSince returns empty when the offset is already at totalSeen", () => {
    const ring = new ByteRing(8);
    ring.append(bytes("hello"));
    expect(ring.bytesSince(ring.totalSeen).length).toBe(0);
    expect(ring.bytesSince(ring.totalSeen + 10).length).toBe(0);
  });

  it("bytesSince returns all retained bytes when the offset fell behind the oldest retained byte", () => {
    const ring = new ByteRing(5);
    ring.append(bytes("abc")); // totalSeen 3, retained "abc"
    ring.append(bytes("defgh")); // totalSeen 8, retained "defgh" (head trimmed)

    // A stale consumer still at offset 1 ("a") fell behind: the bytes between
    // offset 1 and the oldest retained byte (offset 3) are gone. The ring hands
    // back everything it still holds so the consumer repaints from the head.
    const all = ring.bytesSince(1);
    expect(text(all)).toBe("defgh");
    expect(all.length).toBe(ring.length);
  });

  it("keeps totalSeen monotonic across many trims", () => {
    const ring = new ByteRing(4);
    let expectedTotal = 0;
    let last = 0;
    for (let i = 0; i < 50; i++) {
      const chunk = bytes("xy");
      ring.append(chunk);
      expectedTotal += chunk.length;
      expect(ring.totalSeen).toBe(expectedTotal);
      expect(ring.totalSeen).toBeGreaterThanOrEqual(last);
      last = ring.totalSeen;
      // Retained length is always capped.
      expect(ring.length).toBeLessThanOrEqual(4);
    }
  });

  it("reconstructs the full stream from incremental bytesSince calls when never falling behind", () => {
    const ring = new ByteRing(6);
    const chunks = ["alpha", "beta", "gamma", "delta"];
    let written = 0;
    let painted = "";
    for (const c of chunks) {
      ring.append(bytes(c));
      const tail = ring.bytesSince(written);
      painted += text(tail);
      written = ring.totalSeen;
    }
    // Because each step consumes exactly the new tail (which is always within
    // the retained window for these small chunks), the painted stream equals
    // the concatenation with no loss/duplication.
    expect(painted).toBe(chunks.join(""));
  });
});
