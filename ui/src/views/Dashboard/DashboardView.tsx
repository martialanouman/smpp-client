import { useEffect } from "react";
import { useTranslation } from "react-i18next";

import { SessionMetrics } from "../../components/SessionMetrics";
import { onSessionsState } from "../../ipc";
import { useMetrics } from "../../store/metrics";
import { usePreferences } from "../../store/preferences";
import { useSessions } from "../../store/sessions";
import { useMetricsFeed } from "../../store/useMetricsFeed";

/**
 * The overview screen: every live session's throughput and window (spec §9.6).
 *
 * It shows the **same** figures as the Sessions screen, through the same
 * component, and does not compute a total of its own. Milestone 011 is what
 * introduces several sessions and with them a meaningful aggregate; adding one
 * here now would mean writing a sum over a list that has at most one element,
 * and then rewriting it when the sharing rules of spec §9 arrive.
 *
 * A profile with no live session is not listed at all. A dashboard that showed
 * a row of zeroes for every profile ever created would bury the one session
 * that is actually running.
 */
export function DashboardView() {
  const { t } = useTranslation();
  const profiles = useSessions((state) => state.profiles);
  const statuses = useSessions((state) => state.statuses);
  const refresh = useSessions((state) => state.refresh);
  const adopt = useSessions((state) => state.adopt);
  const latest = useMetrics((state) => state.latest);
  const history = useMetrics((state) => state.history);
  const notify = usePreferences((state) => state.notify);

  useMetricsFeed();

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    onSessionsState((payload) => adopt(payload.sessions))
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

  const live = profiles.filter((profile) => {
    const sessionId = profile.sessionId;

    return sessionId !== null && latest[sessionId] !== undefined;
  });

  return (
    <div className="flex max-w-3xl flex-col gap-6">
      <p className="text-sm opacity-70">{t("screen.dashboard")}</p>

      {live.length === 0 ? (
        <p className="rounded-md border border-dashed border-[var(--shinobi-border)] p-6 text-sm">
          {t("metrics.noLiveSession")}
        </p>
      ) : (
        <ul className="flex flex-col gap-4">
          {live.map((profile) => {
            const sessionId = profile.sessionId ?? "";
            const tick = latest[sessionId];

            if (!tick) {
              return null;
            }

            return (
              <li key={sessionId} className="rounded-md border border-[var(--shinobi-border)] p-4">
                <div className="mb-3 flex flex-wrap items-baseline gap-2">
                  <span className="font-medium">{profile.name}</span>
                  <span className="text-sm opacity-70">
                    {profile.host}:{profile.port}
                  </span>
                  <span className="ml-auto text-sm opacity-70">
                    {t(`sessions.state.${statuses[sessionId]?.state ?? "CLOSED"}`)}
                  </span>
                </div>

                <SessionMetrics tick={tick} history={history[sessionId]} />
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
