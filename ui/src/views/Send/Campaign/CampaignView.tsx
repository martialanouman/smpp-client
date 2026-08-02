import { useEffect } from "react";
import { useTranslation } from "react-i18next";

import { onCampaignProgress } from "../../../ipc";
import { useCampaigns } from "../../../store/campaign";
import { useContacts } from "../../../store/contacts";
import { usePreferences } from "../../../store/preferences";
import { useSessions } from "../../../store/sessions";
import { CampaignForm } from "./CampaignForm";
import { CampaignPanel } from "./CampaignPanel";

/**
 * Send › Campagne (deliverable L-010-08, spec §10.2).
 *
 * The form on top because an empty screen is what an operator arrives at, and
 * the campaigns under it because that is what they come back to.
 *
 * # Three stores, and none of them duplicates another
 *
 * * `useCampaigns` — the campaigns, their live counters **and their
 *   throughput**, all three arriving on `campaign:progress`;
 * * `useContacts` — the lists the form selects a recipient set from.
 *
 * `useMetrics` is deliberately absent. It carries the **session's** rate, which
 * counts every submission on the link — a unit send made while a campaign runs
 * is inside it — so beside a campaign's counters it would be a second number
 * describing something else. It belongs on the Sessions and Dashboard screens,
 * where it is labelled as the session's (spec §15.3, ADR 0015).
 */
export function CampaignView() {
  const { t } = useTranslation();

  const profiles = useSessions((state) => state.profiles);
  const statuses = useSessions((state) => state.statuses);

  const rows = useCampaigns((state) => state.rows);
  const progress = useCampaigns((state) => state.progress);
  const creating = useCampaigns((state) => state.creating);
  const reload = useCampaigns((state) => state.reload);
  const create = useCampaigns((state) => state.create);
  const start = useCampaigns((state) => state.start);
  const pause = useCampaigns((state) => state.pause);
  const resume = useCampaigns((state) => state.resume);
  const cancel = useCampaigns((state) => state.cancel);
  const applyProgress = useCampaigns((state) => state.applyProgress);

  const lists = useContacts((state) => state.lists);
  const loadReferences = useContacts((state) => state.loadReferences);
  const notify = usePreferences((state) => state.notify);

  useEffect(() => {
    void reload();
    void loadReferences();
  }, [reload, loadReferences]);

  useEffect(() => {
    // The unlisten function arrives asynchronously and the component can
    // unmount before it does; without the flag a fast unmount leaks a listener
    // that keeps firing on a dead store.
    let live = true;
    let unlisten: (() => void) | undefined;

    onCampaignProgress(applyProgress)
      .then((stop) => {
        if (live) {
          unlisten = stop;
        } else {
          stop();
        }
      })
      // Subscribing rejects whenever the Tauri API is unavailable — opening the
      // dev server in a plain browser is enough — and an unhandled rejection
      // here would take the whole screen down over a progress bar. The form,
      // the list and the four controls all work without this listener; only
      // the live counters stop moving. Same reasoning as `ContactsView`.
      .catch(() => {
        notify({ code: null, message: "campaign progress events are unavailable" });
      });

    return () => {
      live = false;
      unlisten?.();
    };
  }, [applyProgress, notify]);

  return (
    <div className="flex flex-col gap-6">
      <p className="max-w-3xl text-sm opacity-70">{t("campaign.intro")}</p>

      <CampaignForm
        profiles={profiles}
        statuses={statuses}
        lists={lists}
        creating={creating}
        onCreate={(input) => void create(input)}
      />

      <section aria-labelledby="campaign-list-heading" className="flex flex-col gap-3">
        <h2 id="campaign-list-heading" className="text-lg font-medium">
          {t("campaign.listHeading")}
        </h2>

        {rows.length === 0 ? (
          <p className="text-sm text-[var(--shinobi-muted)]">{t("campaign.empty")}</p>
        ) : (
          rows.map((campaign) => {
            return (
              <CampaignPanel
                key={campaign.campaignId}
                campaign={campaign}
                progress={progress[campaign.campaignId]}
                onStart={() => void start(campaign.campaignId)}
                onPause={() => void pause(campaign.campaignId)}
                onResume={() => void resume(campaign.campaignId)}
                onCancel={() => void cancel(campaign.campaignId)}
              />
            );
          })
        )}
      </section>
    </div>
  );
}
