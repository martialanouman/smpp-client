interface GaugeProps {
  /** What is being measured, already translated. */
  readonly label: string;
  /** The reading, already formatted for display. */
  readonly reading: string;
  /** How full the bar is, in `0..=1`. */
  readonly fraction: number;
  /**
   * The reading spoken to assistive technology.
   *
   * Separate from {@link reading} because a bar reading "82 / 100" is clear
   * beside its label and meaningless read out on its own.
   */
  readonly description: string;
  /** Whether to draw the bar in the warning colour. */
  readonly alert?: boolean;
}

/** Keeps a fraction inside `0..=1`, whatever arithmetic produced it. */
function clamp(fraction: number): number {
  if (!Number.isFinite(fraction)) {
    return 0;
  }

  return Math.min(1, Math.max(0, fraction));
}

/**
 * A labelled bar with its numeric reading (spec §9.6).
 *
 * **The number is always there, next to the bar.** A gauge whose only output
 * is a length cannot be read by someone who cannot see it, and cannot be read
 * *precisely* by anyone — spec §16.4 asks for accessibility, and "the bar
 * looks about three-quarters full" is not a throughput figure.
 *
 * The element carries `role="meter"` with its ARIA range, so a screen reader
 * announces the value rather than describing a `<div>`. `aria-valuetext`
 * carries the human phrasing; without it the reading is announced as a bare
 * fraction between nought and one.
 */
export function Gauge({ label, reading, fraction, description, alert = false }: GaugeProps) {
  const value = clamp(fraction);

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-xs opacity-70">{label}</span>
        <span className="font-mono text-xs tabular-nums">{reading}</span>
      </div>

      <div
        role="meter"
        aria-label={label}
        aria-valuenow={Math.round(value * 100)}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuetext={description}
        className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--shinobi-hover)]"
      >
        <div
          // `transition-none` on purpose: the tick arrives four times a second
          // and a CSS transition longer than 250 ms would still be animating
          // when the next value lands, so the bar would lag the figure beside
          // it — two readings of the same thing, disagreeing.
          className={`h-full transition-none ${
            alert ? "bg-amber-500" : "bg-[var(--shinobi-accent)]"
          }`}
          style={{ width: `${value * 100}%` }}
        />
      </div>
    </div>
  );
}
