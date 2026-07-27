import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Row virtualisation for the log table (CA-008-07).
 *
 * # Why this is thirty lines and not a dependency
 *
 * `@tanstack/react-virtual` is what CLAUDE.md §2 lists and what step-008 §2
 * names, and it was tried first. Two things stopped it, and both are recorded
 * in ADR 0011:
 *
 * * the React Compiler **refuses to compile** a component using it — the
 *   plugin reports "returns functions which cannot be memoized without leading
 *   to stale UI" — so the one screen that most needs memoisation is the one
 *   that loses it;
 * * under this project's own test environment it never re-renders after mount,
 *   so the window stays empty and **the criterion could not be tested at all**.
 *   Shipping a virtualised table whose virtualisation no test exercises is
 *   worse than owning thirty lines.
 *
 * The behaviour that matters is a division and a slice at a fixed row height.
 * [`rowWindow`] is that arithmetic, as a pure function — so the property
 * CA-008-07 is about ("what is rendered does not grow with the number of
 * rows") is a unit test over numbers rather than a guess about a DOM nobody
 * can measure.
 *
 * # What is deliberately not here
 *
 * Variable row heights. The table's cells are single-line by construction, so
 * every row is the same height, and measuring each one is the cost this exists
 * to avoid.
 */

/** Which rows to render, and where to put them. */
export interface RowWindow {
  /** Index of the first row to render. */
  readonly start: number;
  /** Index **past** the last row to render. */
  readonly end: number;
  /** Pixels of empty space standing in for the rows before [`start`]. */
  readonly offsetTop: number;
  /** Height of the whole list, which is what sizes the scrollbar. */
  readonly totalHeight: number;
}

/**
 * The rows visible at `scrollTop`, plus `overscan` on each side.
 *
 * Pure, and total: every argument is clamped, so a negative scroll offset
 * (elastic scrolling on macOS), a zero viewport (before the first layout) and a
 * count of zero all produce an empty-but-valid window rather than a slice with
 * `start > end`.
 */
export function rowWindow(
  count: number,
  rowHeight: number,
  scrollTop: number,
  viewportHeight: number,
  overscan: number,
): RowWindow {
  const height = Math.max(rowHeight, 1);
  const total = Math.max(count, 0);
  const offset = Math.max(scrollTop, 0);
  const viewport = Math.max(viewportHeight, 0);

  const first = Math.max(Math.floor(offset / height) - overscan, 0);
  const visible = Math.ceil(viewport / height) + overscan * 2;
  const last = Math.min(first + visible, total);

  return {
    start: Math.min(first, total),
    end: last,
    offsetTop: Math.min(first, total) * height,
    totalHeight: total * height,
  };
}

/** Rows rendered beyond each edge of the viewport, so a scroll shows content. */
const OVERSCAN = 8;

/**
 * Tracks a scroll container and reports the window to render.
 *
 * Returns the ref to put on the scrolling element. The measurement is taken
 * from `clientHeight` and `scrollTop` — no `ResizeObserver`, which jsdom does
 * not implement and which buys nothing here: the table fills its pane, and the
 * `resize` event covers the window being resized.
 */
export function useRowWindow(
  count: number,
  rowHeight: number,
): {
  readonly ref: React.RefObject<HTMLDivElement | null>;
  readonly window: RowWindow;
  /** To put on the container's `onScroll`. */
  readonly onScroll: () => void;
} {
  const ref = useRef<HTMLDivElement>(null);
  const [scroll, setScroll] = useState({ top: 0, height: 0 });

  const measure = useCallback(() => {
    const element = ref.current;

    if (element === null) {
      return;
    }

    setScroll((current) =>
      // Only when something moved: `setState` with an equal object would
      // re-render on every scroll event, which is the cost virtualisation
      // exists to avoid.
      current.top === element.scrollTop && current.height === element.clientHeight
        ? current
        : { top: element.scrollTop, height: element.clientHeight },
    );
  }, []);

  useEffect(() => {
    // The first measurement has to happen after layout, which is why it is an
    // effect and not a value read during render: at render time the ref is
    // still null and the element has no height.
    measure();

    window.addEventListener("resize", measure);

    return () => {
      window.removeEventListener("resize", measure);
    };
  }, [measure]);

  return {
    ref,
    window: rowWindow(count, rowHeight, scroll.top, scroll.height, OVERSCAN),
    onScroll: measure,
  };
}
