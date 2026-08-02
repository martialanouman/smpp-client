import { useTranslation } from "react-i18next";

import { Gauge } from "../../../components/Gauge";
import type { CampaignProgressEvent, CampaignRowDto } from "../../../ipc";

/** Statuses from which a campaign can be started for the first time. */
const STARTABLE = ["VALIDATED", "CREATED"];

/** Statuses a campaign no longer moves from. */
const TERMINAL = ["COMPLETED", "CANCELLED", "FAILED"];

interface Props {
  readonly campaign: CampaignRowDto;
  /** The latest reading, or `undefined` before the first one. */
  readonly progress: CampaignProgressEvent | undefined;
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
 * restart — the row's own counters are what there is. The throughput has no
 * such fallback and reads as unknown, which is what it is: a rate is a
 * measurement over a window, and a campaign nobody is watching has none.
 */
export function CampaignPanel({ campaign, progress, onStart, onPause, onResume, onCancel }: Props) {
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
        {/*
          NO "Délivrés" counter, although `campaigns.delivered_count` exists and
          `CampaignRowDto` carries it.

          Nothing in this workspace feeds that column: a delivery receipt is
          correlated to one message (milestone 008) and nobody aggregates
          receipts back onto the campaign. Shown here it read as a permanent
          zero in the same grid as five exact figures — "Acceptés 200 000 ·
          Échecs 0 · Délivrés 0" says the message centre took everything and
          nothing arrived, which is an incident report waiting to be opened
          against an operator who did nothing wrong.

          A figure that means "not measured" beside figures that are measured is
          worse than no figure. It comes back with the statistics of milestone
          014, which is what will make it true.
        */}
        <div className="flex flex-col">
          <dt className="opacity-70">{t("campaign.throughput")}</dt>
          {/*
            The CAMPAIGN's rate, measured in the backend from its own
            acceptances — not `metrics:tick`, which measures the whole session
            and would fold a unit send made beside the campaign into the figure
            shown next to that campaign's counters (spec §15.3, ADR 0015).
          */}
          <dd className="font-mono tabular-nums">{rate(progress?.acceptedPerSecond)}</dd>
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

      {/*
        The terminal status is checked FIRST, before `live`. A campaign the
        operator has just cancelled is still live — it drains its queue and
        journals what is in flight before its task returns — and its readings
        carry `CANCELLED` from the moment the cancellation lands. Testing `live`
        first would offer *Mettre en pause* and *Annuler* on a campaign that is
        already stopping.
      */}
      <footer className="flex flex-wrap gap-2">
        {terminal ? null : campaign.live ? (
          <>
            {campaign.status === "PAUSED" ? (
              <Action label={t("campaign.resume")} onClick={onResume} />
            ) : (
              <Action label={t("campaign.pause")} onClick={onPause} />
            )}
            <Action label={t("campaign.cancel")} onClick={onCancel} />
          </>
        ) : interrupted ? (
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
