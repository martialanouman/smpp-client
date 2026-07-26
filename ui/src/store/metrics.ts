import { create } from "zustand";

import type { MetricsTick } from "../ipc";

/**
 * How many samples of history one session keeps, per series.
 *
 * At the backend's 4 Hz that is thirty seconds — enough for a curve to show a
 * trend and to make a stall obvious.
 *
 * **A hard bound, not a hint.** The tick never stops while a session is bound,
 * so an unbounded array here would grow for as long as the application is
 * open: fourteen thousand samples an hour, per session, in the WebView's
 * memory. CA-007-06 is about the backend, but the same failure is available on
 * this side for free, and it would show up as an interface that gets slower
 * the longer it runs.
 */
export const HISTORY_LENGTH = 120;

/**
 * What one session's curve is drawn from.
 *
 * Two series rather than one: the one-second rate is what an operator watches
 * live, and the window occupancy is what explains it — a throughput that has
 * flattened with a full window is latency-bound, and one with an empty window
 * is limited by the quota. Read together they say *why*, which neither says
 * alone.
 */
export interface MetricsHistory {
  /** Sliding one-second throughput, oldest first. */
  readonly tps: readonly number[];
  /** Window occupancy in `0..=1`, oldest first. */
  readonly occupancy: readonly number[];
}

interface MetricsState {
  /** The most recent tick, keyed by `sessionId`. */
  readonly latest: Readonly<Record<string, MetricsTick>>;
  /** Bounded history, keyed by `sessionId`. */
  readonly history: Readonly<Record<string, MetricsHistory>>;
  /** Adopts one `metrics:tick` payload. */
  readonly adopt: (tick: MetricsTick) => void;
  /** Forgets a session — called when its profile is deleted. */
  readonly forget: (sessionId: string) => void;
}

/** Appends `value` and keeps at most {@link HISTORY_LENGTH} samples. */
function append(series: readonly number[], value: number): readonly number[] {
  const next = [...series, value];

  return next.length > HISTORY_LENGTH ? next.slice(next.length - HISTORY_LENGTH) : next;
}

/**
 * The live figures of every session.
 *
 * Deliberately separate from `useSessions`. The two channels have completely
 * different rates — `sessions:state` fires on a transition, `metrics:tick`
 * four times a second — and a single store would repaint the profile list, the
 * forms and the bind buttons on every tick. Zustand re-renders per selector,
 * so keeping them apart is what stops a gauge animation from touching anything
 * else on the screen (ENF-PERF-03).
 *
 * **No derived averages here.** The backend computes the sliding windows at
 * full rate; recomputing them from the four samples a second that reach this
 * side would make their accuracy depend on the display cadence, which is
 * exactly what the tick's throttling is meant to be free to change.
 */
export const useMetrics = create<MetricsState>((set) => ({
  latest: {},
  history: {},

  adopt: (tick) => {
    set((state) => {
      const previous = state.history[tick.sessionId] ?? { tps: [], occupancy: [] };

      return {
        latest: { ...state.latest, [tick.sessionId]: tick },
        history: {
          ...state.history,
          [tick.sessionId]: {
            // A rate the backend has not measured yet is charted as zero:
            // the curve needs a point per tick to keep its time axis even, and
            // a gap would silently compress the elapsed time. The distinction
            // that matters — no figure versus a real zero — is kept where it is
            // read, in the gauges.
            tps: append(previous.tps, tick.tps1s ?? 0),
            occupancy: append(previous.occupancy, tick.windowOccupancy ?? 0),
          },
        },
      };
    });
  },

  forget: (sessionId) => {
    set((state) => {
      const latest = { ...state.latest };
      const history = { ...state.history };

      delete latest[sessionId];
      delete history[sessionId];

      return { latest, history };
    });
  },
}));
