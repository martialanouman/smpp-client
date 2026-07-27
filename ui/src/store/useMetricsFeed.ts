import { useEffect } from "react";

import { onMetricsTick } from "../ipc";
import { useMetrics } from "./metrics";
import { usePreferences } from "./preferences";

/**
 * Subscribes the metrics store to `metrics:tick` for as long as the caller is
 * mounted.
 *
 * A hook rather than a subscription inside the store, because the teardown is
 * the whole point: two screens show these figures, and a store that subscribed
 * itself would either subscribe twice — every tick adopted twice — or never
 * unsubscribe, leaving a listener firing into a reducer nobody reads.
 *
 * A failed subscription is **notified, not thrown**. The screen around it
 * still lists sessions and still runs commands; only the live figures stop
 * updating, and a thrown promise here would take the whole view down with it.
 * `SessionsView` treats `sessions:state` the same way, for the same reason.
 */
export function useMetricsFeed(): void {
  const adopt = useMetrics((state) => state.adopt);
  const notify = usePreferences((state) => state.notify);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    onMetricsTick(adopt)
      .then((stop) => {
        if (cancelled) {
          stop();
        } else {
          unlisten = stop;
        }
      })
      .catch((cause: unknown) => {
        notify({
          code: null,
          message: cause instanceof Error ? cause.message : String(cause),
        });
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [adopt, notify]);
}
