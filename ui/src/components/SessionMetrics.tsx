import { useTranslation } from "react-i18next";

import type { MetricsTick } from "../ipc";
import type { MetricsHistory } from "../store/metrics";
import { Gauge } from "./Gauge";
import { Sparkline } from "./Sparkline";

/** Occupancy above which the window gauge switches to its warning colour. */
const WINDOW_ALERT = 0.9;

interface SessionMetricsProps {
  /** The most recent tick for this session. */
  readonly tick: MetricsTick;
  /** Its bounded history, for the curve. */
  readonly history: MetricsHistory | undefined;
}

/**
 * A rate, with as many decimals as it is worth showing.
 *
 * `null` is not zero and must not be rendered as such: the backend sends it
 * when a session has produced no submission yet, and "0.0 TPS" on a session
 * that has not started reads as a stalled link rather than an idle one. An
 * em dash is the honest rendering of "no figure yet".
 */
function rate(value: number | null): string {
  if (value === null) {
    return "—";
  }

  return value >= 100 ? value.toFixed(0) : value.toFixed(1);
}

/**
 * The throughput and window gauges of one session (spec §9.6, §18.1).
 *
 * Shared by the Sessions screen and the Dashboard rather than written twice:
 * the two show the same figures, and a second copy is a second place for a
 * unit or a rounding rule to drift.
 *
 * # What is shown and what is not
 *
 * The throughput gauge is drawn against `targetTps` — the configured ceiling —
 * because a bar with no scale is a number in a costume. A session with no
 * limit has no ceiling to draw against, so the fraction is taken against the
 * peak the session has actually reached, which is the only honest scale
 * available.
 *
 * Every rate is nullable, and the difference matters: `null` means the session
 * has submitted nothing yet, `0` means it submitted and the rate has since
 * fallen to zero. The first is a session waiting for work, the second may be a
 * session that has stopped moving.
 */
export function SessionMetrics({ tick, history }: SessionMetricsProps) {
  const { t } = useTranslation();

  const unlimited = tick.targetTps === 0;
  // `?? 0` only feeds the scale, never the reading: a missing peak makes the
  // bar empty, it does not turn into a displayed zero.
  const scale = unlimited ? Math.max(tick.tpsPeak ?? 0, 1) : tick.targetTps;
  const occupancy = tick.windowOccupancy ?? 0;

  return (
    <div className="flex flex-col gap-3">
      <div className="grid gap-3 sm:grid-cols-2">
        <Gauge
          label={t("metrics.throughput")}
          reading={
            unlimited
              ? t("metrics.tpsUnlimited", { value: rate(tick.tps1s) })
              : t("metrics.tpsOfTarget", { value: rate(tick.tps1s), target: tick.targetTps })
          }
          fraction={(tick.tps1s ?? 0) / scale}
          description={t("metrics.throughputSpoken", { value: rate(tick.tps1s) })}
          alert={tick.backingOff}
        />

        <Gauge
          label={t("metrics.window")}
          reading={t("metrics.windowOf", { value: tick.windowInUse, total: tick.windowSize })}
          fraction={occupancy}
          description={t("metrics.windowSpoken", {
            value: tick.windowInUse,
            total: tick.windowSize,
          })}
          alert={occupancy >= WINDOW_ALERT}
        />
      </div>

      <Sparkline
        series={history?.tps ?? []}
        label={t("metrics.curve")}
        // `exactOptionalPropertyTypes` is on: an unlimited session must omit
        // the prop, not pass `undefined` through it.
        {...(unlimited ? {} : { ceiling: tick.targetTps })}
      />

      {/* A definition list rather than a table: these are labelled scalars
          about one subject, which is what `<dl>` is for, and a table would
          announce a row and column position that means nothing here. */}
      <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs sm:grid-cols-4">
        <Figure label={t("metrics.tps10s")} value={rate(tick.tps10s)} />
        <Figure label={t("metrics.tpsAverage")} value={rate(tick.tpsAverage)} />
        <Figure label={t("metrics.tpsPeak")} value={rate(tick.tpsPeak)} />
        <Figure
          label={t("metrics.rtt")}
          value={tick.rttMs === null ? "—" : t("metrics.ms", { value: tick.rttMs.toFixed(1) })}
        />
        <Figure label={t("metrics.accepted")} value={String(tick.accepted)} />
        <Figure label={t("metrics.rejected")} value={String(tick.rejected)} />
        <Figure label={t("metrics.timedOut")} value={String(tick.timedOut)} />
        <Figure label={t("metrics.uptime")} value={t("metrics.seconds", { value: tick.uptimeS })} />
        <Figure label={t("metrics.reconnects")} value={String(tick.reconnects)} />
        <Figure label={t("metrics.throttled")} value={String(tick.throttled)} />
      </dl>

      {tick.backingOff ? (
        <p className="text-xs text-amber-700 dark:text-amber-300">{t("metrics.backingOff")}</p>
      ) : null}
    </div>
  );
}

interface FigureProps {
  readonly label: string;
  readonly value: string;
}

function Figure({ label, value }: FigureProps) {
  return (
    <div className="flex flex-col">
      <dt className="opacity-70">{label}</dt>
      <dd className="font-mono tabular-nums">{value}</dd>
    </div>
  );
}
