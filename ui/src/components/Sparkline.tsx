interface SparklineProps {
  /** Samples, oldest first. */
  readonly series: readonly number[];
  /** Described to assistive technology, already translated. */
  readonly label: string;
  /**
   * The upper bound of the vertical axis.
   *
   * Omit to scale to the tallest sample. Pass the configured target to draw
   * every curve against the same scale, which is what makes two sessions
   * comparable at a glance.
   */
  readonly ceiling?: number;
}

/** Width of the drawing area, in the SVG's own units. */
const WIDTH = 240;

/** Height of the drawing area, in the SVG's own units. */
const HEIGHT = 40;

/**
 * A real-time curve (spec §9.6).
 *
 * Plain SVG, no charting library. The whole requirement is a polyline over at
 * most a hundred and twenty points refreshed four times a second; a charting
 * dependency would be several hundred kilobytes in the WebView for a `<path>`,
 * and CLAUDE.md §2 asks a new dependency to justify itself.
 *
 * `preserveAspectRatio="none"` with a `viewBox`: the curve stretches to
 * whatever width its container has, so nothing here has to measure the layout.
 *
 * # Accessibility
 *
 * `role="img"` with a label, and **no** attempt to expose the points. A
 * hundred and twenty numbers read out is not information; the figures that
 * matter are on the gauges beside it, which are readable individually.
 */
export function Sparkline({ series, label, ceiling }: SparklineProps) {
  const samples = series.length;
  const highest = Math.max(ceiling ?? 0, ...series, 1);

  // A single point has no line to draw. Two identical points do, and it is a
  // flat line at the right height, which is the honest picture of a session
  // sending at a steady rate.
  const points =
    samples < 2
      ? ""
      : series
          .map((value, index) => {
            const x = (index / (samples - 1)) * WIDTH;
            const y = HEIGHT - (Math.max(0, value) / highest) * HEIGHT;

            return `${x.toFixed(1)},${y.toFixed(1)}`;
          })
          .join(" ");

  return (
    <svg
      role="img"
      aria-label={label}
      viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
      preserveAspectRatio="none"
      className="h-10 w-full rounded-sm bg-[var(--shinobi-hover)]/40"
    >
      {points ? (
        <polyline
          points={points}
          fill="none"
          stroke="var(--shinobi-accent)"
          strokeWidth="1.5"
          vectorEffect="non-scaling-stroke"
          strokeLinejoin="round"
        />
      ) : null}
    </svg>
  );
}
