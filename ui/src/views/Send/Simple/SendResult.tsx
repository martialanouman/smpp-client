import { useTranslation } from "react-i18next";

import type { MessageSendResultDto } from "../../../ipc";

interface Props {
  readonly result: MessageSendResultDto | null;
  /** The state `message:update` last reported, which may be ahead of nothing. */
  readonly progress: string | null;
}

/** The colour of a lifecycle badge (spec §14.3). */
function badgeClass(state: string): string {
  switch (state) {
    case "ACCEPTED":
    case "DELIVERED":
      return "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300";
    case "QUEUED":
    case "SENT":
      return "bg-amber-500/15 text-amber-700 dark:text-amber-300";
    case "FAILED":
    case "EXPIRED":
      return "bg-red-500/15 text-red-700 dark:text-red-300";
    default:
      return "bg-[var(--shinobi-hover)] opacity-80";
  }
}

/**
 * What became of the last message (CA-006-01, CA-006-05).
 *
 * The `command_status` is shown **as the message centre sent it**: its number,
 * its symbolic name and its label, all three from the backend's own table
 * (ENF-UTI-02). Nothing is rephrased here — an operator quoting
 * `ESME_RTHROTTLED` to their provider needs the string the provider uses, not
 * a friendlier one.
 *
 * The per-segment table only appears past one segment: for a short message it
 * would repeat the header.
 */
export function SendResult({ result, progress }: Props) {
  const { t } = useTranslation();

  // The panel appears as soon as the **first** transition arrives, before the
  // command has returned. Waiting for the result would collapse
  // `QUEUED → SENT → ACCEPTED` into a single repaint at the end, which is
  // exactly what CA-006-01 asks the operator to be able to watch, and what the
  // `message:update` event exists for.
  if (result === null && progress === null) {
    return null;
  }

  const state = progress ?? result?.state ?? "QUEUED";

  return (
    <section
      aria-live="polite"
      className="mt-8 flex max-w-3xl flex-col gap-3 rounded-md border border-[var(--shinobi-border)] p-4"
    >
      <div className="flex flex-wrap items-center gap-3">
        <h2 className="text-sm font-semibold">{t("send.result.title")}</h2>

        <span className={`rounded px-2 py-0.5 text-xs font-medium ${badgeClass(state)}`}>
          {t(`send.state.${state}`)}
        </span>

        {result === null ? null : (
          <span className="text-sm opacity-70">
            {t("send.result.segments", { count: result.segments })}
          </span>
        )}
      </div>

      <dl className="grid gap-x-6 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
        <dt className="opacity-70">{t("send.result.clientMessageId")}</dt>
        <dd className="font-mono text-xs">{result?.clientMessageId ?? "—"}</dd>

        {result?.smscMessageId == null ? null : (
          <>
            <dt className="opacity-70">{t("send.result.smscMessageId")}</dt>
            <dd className="font-mono text-xs">{result.smscMessageId}</dd>
          </>
        )}

        {result === null ? null : result.commandStatus === null ? (
          <>
            <dt className="opacity-70">{t("send.result.status")}</dt>
            <dd>{t("send.result.noAnswer")}</dd>
          </>
        ) : (
          <>
            <dt className="opacity-70">{t("send.result.status")}</dt>
            <dd>
              <span className="font-mono text-xs">
                {result.statusSymbol ?? t("send.result.unknownStatus")} (
                {`0x${result.commandStatus.toString(16).toUpperCase().padStart(8, "0")}`})
              </span>
              {result.statusLabel === null ? null : (
                <span className="opacity-80"> — {result.statusLabel}</span>
              )}
            </dd>
          </>
        )}
      </dl>

      {result?.statusIsVendorSpecific ? (
        <p className="text-xs opacity-70">{t("send.result.vendorSpecific")}</p>
      ) : null}

      {result?.retryable ? (
        <p className="text-xs opacity-70">{t("send.result.retryable")}</p>
      ) : null}

      {result !== null && result.segments > 1 ? (
        <div className="overflow-x-auto">
          <table className="mt-2 w-full text-left text-xs">
            <thead className="opacity-70">
              <tr>
                <th className="py-1 pr-4 font-medium">{t("send.result.segment")}</th>
                <th className="py-1 pr-4 font-medium">{t("send.result.outcome")}</th>
                <th className="py-1 pr-4 font-medium">{t("send.result.status")}</th>
                <th className="py-1 font-medium">{t("send.result.smscMessageId")}</th>
              </tr>
            </thead>
            <tbody>
              {result.outcomes.map((outcome) => (
                <tr
                  key={outcome.sequenceNumber}
                  className="border-t border-[var(--shinobi-border)]"
                >
                  <td className="py-1 pr-4">{outcome.sequenceNumber}</td>
                  <td className="py-1 pr-4">{t(`send.outcomes.${outcome.outcome}`)}</td>
                  <td className="py-1 pr-4 font-mono">{outcome.statusSymbol ?? "—"}</td>
                  <td className="py-1 font-mono">{outcome.smscMessageId ?? "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </section>
  );
}
