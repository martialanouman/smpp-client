import { describe, expect, it } from "vitest";

import { rowWindow } from "./rowWindow";

const ROW = 36;
const VIEWPORT = 600;
const OVERSCAN = 8;

/** The window at the top of a list of `count` rows. */
function atTop(count: number) {
  return rowWindow(count, ROW, 0, VIEWPORT, OVERSCAN);
}

describe("rowWindow", () => {
  /**
   * **CA-008-07, stated as arithmetic.** What gets rendered must not grow with
   * the number of rows — that is the whole of virtualisation, and it is a
   * property of numbers rather than of a DOM.
   *
   * Ten rows, two hundred thousand rows, two million rows: the same window.
   */
  it("renders the same number of rows whatever the total", () => {
    const small = atTop(100);
    const large = atTop(200_000);
    const enormous = atTop(2_000_000);

    expect(large.end - large.start).toBe(small.end - small.start);
    expect(enormous.end - enormous.start).toBe(small.end - small.start);
    expect(large.end - large.start).toBeLessThan(40);
  });

  /** The scrollbar is sized from the total, or it could not reach the end. */
  it("sizes the list from the total, not from the window", () => {
    expect(atTop(200_000).totalHeight).toBe(200_000 * ROW);
  });

  /** Scrolling moves the window, and the spacer moves with it. */
  it("moves the window and its offset together as the list is scrolled", () => {
    const scrolled = rowWindow(200_000, ROW, 100 * ROW, VIEWPORT, OVERSCAN);

    expect(scrolled.start).toBe(100 - OVERSCAN);
    expect(scrolled.offsetTop).toBe((100 - OVERSCAN) * ROW);
    // The spacer must place the first rendered row exactly where the scroll
    // position expects it; an offset computed from `start + overscan` would
    // put every row eight rows too low.
    expect(scrolled.offsetTop).toBe(scrolled.start * ROW);
  });

  /** The overscan is what stops a fast scroll from showing blank rows. */
  it("renders rows on both sides of the viewport", () => {
    const scrolled = rowWindow(1_000, ROW, 50 * ROW, VIEWPORT, OVERSCAN);
    const visible = Math.ceil(VIEWPORT / ROW);

    expect(scrolled.start).toBeLessThan(50);
    expect(scrolled.end).toBeGreaterThan(50 + visible);
  });

  /** At the end of the list the window stops rather than running past it. */
  it("never runs past the last row", () => {
    const atEnd = rowWindow(20, ROW, 20 * ROW, VIEWPORT, OVERSCAN);

    expect(atEnd.end).toBe(20);
    expect(atEnd.start).toBeLessThanOrEqual(atEnd.end);
  });

  /**
   * The degenerate inputs, each of which really occurs:
   *
   * * a zero viewport — the first render, before layout;
   * * a negative offset — elastic scrolling on macOS;
   * * an empty list — a filter that matches nothing.
   *
   * None may produce a slice with `start > end`, which `Array.slice` would
   * silently turn into an empty table on a list that has rows.
   */
  it("stays coherent on a zero viewport, a negative offset and an empty list", () => {
    for (const window of [
      rowWindow(500, ROW, 0, 0, OVERSCAN),
      rowWindow(500, ROW, -400, VIEWPORT, OVERSCAN),
      rowWindow(0, ROW, 0, VIEWPORT, OVERSCAN),
      rowWindow(0, 0, 0, 0, 0),
    ]) {
      expect(window.start).toBeLessThanOrEqual(window.end);
      expect(window.start).toBeGreaterThanOrEqual(0);
      expect(window.offsetTop).toBeGreaterThanOrEqual(0);
      expect(window.totalHeight).toBeGreaterThanOrEqual(0);
    }
  });

  /** A zero viewport still renders the overscan, so the table is not blank
   * during the frame before the first measurement. */
  it("renders the overscan even before the viewport has been measured", () => {
    const unmeasured = rowWindow(500, ROW, 0, 0, OVERSCAN);

    expect(unmeasured.end).toBeGreaterThan(0);
  });
});
