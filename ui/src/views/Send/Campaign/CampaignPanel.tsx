import { useTranslation } from "react-i18next";

import { Gauge } from "../../../components/Gauge";
import type { CampaignProgressEvent, CampaignRowDto, MetricsTick } from "../../../ipc";

/** Statuses from which a campaign can be started for the first time. */
const STARTABLE = ["VALIDATED", "CREATED"];

/** Statuses a campaign no longer moves from. */
const TERMINAL = ["COMPLETED", "CANCELLED", "FAILED"];

interface Props {
  readonly campaign: CampaignRowDto;
  /** The latest reading, or `undefined` before the first one. */
  readonly progress: CampaignProgressEvent | undefined;
  /**
   * The live figures of the session this campaign sends on.
   *
   * **Where the throughput comes from.** `metrics:tick`, milestone 007, which
   * measures the session's sliding rate at full speed. Nothing here derives a
   * rate from the campaign's counters: four readings a second are enough to
   * draw a bar and not enough to measure a rate, and a second figure would
   * disagree with the gauges on the Sessions and Dashboard screens.
   *
   * The honest caveat, and it is shown next to the figure: this is the rate of
   * the **session**, so a unit send made on the same session while a campaign
   * runs is counted in it. At this milestone a campaign owns its session for
   * the length of its run, so that is a corner rather than the normal case.
   */
  readonly metrics: MetricsTick | undefined;
  readonly onStart: () => void;
  readonly onPause: () => void;
  readonly onResume: () => void;
  readonly onCancel: () => void;
}

/** A rate, with as many decimals as it is worth showing. */
function rate(value: number | null | undefined): string {
  if (value === null || value === undefined) {
    return "—";
  }

  return value >= 100 ? value.toFixed(0) : value.toFixed(1);
}

/**
 * One campaign: where it stands, how far it has got, and what can be done to it
 * (deliverable L-010-08).
 *
 * # Which buttons are offered, and why `live` decides it
 *
 * A campaign's row carries both a **status** and a **`live`** flag, and the two
 * disagree in exactly the case that matters: a process killed mid-campaign
 * leaves a row reading `RUNNING` with nothing behind it. Offering Pause there
 * would be offering to suspend something that is not running; what that campaign
 * needs is *Reprendre*, which starts a fresh run in resuming mode and sends
 * nothing already accepted (CA-010-05).
 *
 * So: Pause and Annuler are offered on a campaign that is **live**; Reprendre on
 * one that is paused or interrupted; Démarrer on one that has never run.
 *
 * # The counters shown are the reading's while it runs, and the row's otherwise
 *
 * `campaign:progress` stops when the campaign does, and its last event carries
 * the final figures. Before the first reading — a campaign listed after a
 * restart — the row's own counters are what there is.
 */
export function CampaignPanel({
  campaign,
  progress,
  metrics,
  onStart,
  onPause,
  onResume,
  onCancel,
}: Props) {
  const { t } = useTranslation();

  const total = progress?.total ?? campaign.total;
  const processed = progress?.processed ?? campaign.sent + campaign.failed;
  const accepted = progress?.accepted ?? campaign.sent;
  const failed = progress?.failed ?? campaign.failed;
  const fraction = total === 0 ? 0 : processed / total;

  const terminal = TERMINAL.includes(campaign.status);
  const interrupted = !campaign.live && !terminal && !STARTABLE.includes(campaign.status);

  return (
    <article className="flex flex-col gap-3 rounded-md border border-[var(--shinobi-border)] p-4">
      <header className="flex flex-wrap items-baseline justify-between gap-2">
        <h3 className="text-base font-medium">{campaign.name}</h3>
        <span className="rounded-full border border-[var(--shinobi-border)] px-2 py-0.5 text-xs">
          {t(`campaign.status.${campaign.status}`, campaign.status)}
        </span>
      </header>

      {interrupted ? (
        <p className="text-xs text-amber-700 dark:text-amber-300">{t("campaign.interrupted")}</p>
      ) : null}

      <Gauge
        label={t("campaign.progress")}
        reading={`${processed} / ${total}`}
        fraction={fraction}
        description={t("campaign.progressSpoken", { processed, total })}
      />

      <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs sm:grid-cols-4">
        <Figure label={t("campaign.counters.accepted")} value={accepted} />
        <Figure label={t("campaign.counters.failed")} value={failed} />
        <Figure label={t("campaign.counters.rejected")} value={progress?.rejected ?? 0} />
        <Figure label={t("campaign.counters.skipped")} value={progress?.skipped ?? 0} />
        <Figure label={t("campaign.counters.cancelled")} value={progress?.cancelled ?? 0} />
        <Figure label={t("campaign.counters.retried")} value={progress?.retried ?? 0} />
        <Figure label={t("campaign.counters.delivered")} value={campaign.delivered} />
        <div className="flex flex-col">
          <dt className="opacity-70">{t("campaign.throughput")}</dt>
          <dd className="font-mono tabular-nums">{rate(metrics?.tps1s)}</dd>
        </div>
      </dl>

      {(progress?.reemittedUnanswered ?? 0) > 0 ? (
        <p className="text-xs text-amber-700 dark:text-amber-300">
          {t("campaign.duplicateRisk", { count: progress?.reemittedUnanswered ?? 0 })}
        </p>
      ) : null}

      {(progress?.notJournalled ?? 0) > 0 ? (
        <p className="text-xs text-amber-700 dark:text-amber-300">
          {t("campaign.notJournalled", { count: progress?.notJournalled ?? 0 })}
        </p>
      ) : null}

      <p className="text-xs opacity-60">{t("campaign.detailHint")}</p>

      <footer className="flex flex-wrap gap-2">
        {campaign.live ? (
          <>
            {campaign.status === "PAUSED" ? (
              <Action label={t("campaign.resume")} onClick={onResume} />
            ) : (
              <Action label={t("campaign.pause")} onClick={onPause} />
            )}
            <Action label={t("campaign.cancel")} onClick={onCancel} />
          </>
        ) : terminal ? null : interrupted ? (
          <>
            <Action label={t("campaign.resume")} onClick={onResume} />
            <Action label={t("campaign.cancel")} onClick={onCancel} />
          </>
        ) : (
          <>
            <Action label={t("campaign.start")} onClick={onStart} />
            <Action label={t("campaign.cancel")} onClick={onCancel} />
          </>
        )}
      </footer>
    </article>
  );
}

/** One counter with its label. */
function Figure({ label, value }: { readonly label: string; readonly value: number }) {
  return (
    <div className="flex flex-col">
      <dt className="opacity-70">{label}</dt>
      <dd className="font-mono tabular-nums">{value}</dd>
    </div>
  );
}

/** One control button. */
function Action({ label, onClick }: { readonly label: string; readonly onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="rounded-md border border-[var(--shinobi-border)] px-3 py-1.5 text-sm"
    >
      {label}
    </button>
  );
}
