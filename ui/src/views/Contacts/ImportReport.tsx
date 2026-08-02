import { useTranslation } from "react-i18next";

import type { ImportReportDto } from "../../ipc";
import { useContacts } from "../../store/contacts";

interface ImportReportProps {
  readonly report: ImportReportDto;
}

/**
 * What an import produced (CA-009-05, CA-009-08).
 *
 * # The arithmetic is shown, not asserted
 *
 * `total = imported + rejected + duplicates` is an invariant of the backend,
 * and the four numbers are shown side by side so an operator can see it hold.
 * A screen that showed only "imported" would hide the two numbers that explain
 * the difference between the file's size and it.
 *
 * # Rejected rows are exportable, not just visible
 *
 * The list is what the operator corrects and re-imports, so it carries the
 * line number and the offending value. The download is assembled here from
 * what the report already holds: writing it through the backend would mean a
 * filesystem capability the application does not take.
 */
export function ImportReport({ report }: ImportReportProps) {
  const { t } = useTranslation();
  const dismiss = useContacts((state) => state.dismissReport);

  const download = () => {
    const header = "line,reason,value\n";
    const body = report.rejectedRows
      .map((row) => `${row.line},${row.reason},${quote(row.value)}`)
      .join("\n");

    const url = URL.createObjectURL(new Blob([header + body], { type: "text/csv" }));
    const anchor = document.createElement("a");

    anchor.href = url;
    anchor.download = "rejected-rows.csv";
    anchor.click();

    // Without this the blob stays alive for the lifetime of the document, and
    // an operator correcting a file in ten passes leaks ten of them.
    URL.revokeObjectURL(url);
  };

  return (
    <section
      aria-labelledby="contacts-report-heading"
      className="flex flex-col gap-3 rounded-md border border-[var(--shinobi-border)] p-4"
    >
      <div className="flex items-baseline justify-between gap-4">
        <h2 id="contacts-report-heading" className="text-lg font-medium">
          {report.cancelled ? t("contacts.report.headingCancelled") : t("contacts.report.heading")}
        </h2>
        <button
          type="button"
          onClick={dismiss}
          className="text-sm text-[var(--shinobi-muted)] hover:underline"
        >
          {t("contacts.report.dismiss")}
        </button>
      </div>

      <dl className="grid grid-cols-2 gap-3 text-sm sm:grid-cols-4">
        <Stat label={t("contacts.report.total")} value={report.total} />
        <Stat label={t("contacts.report.imported")} value={report.imported} />
        <Stat label={t("contacts.report.rejected")} value={report.rejected} />
        <Stat label={t("contacts.report.duplicates")} value={report.duplicates} />
        <Stat label={t("contacts.report.blank")} value={report.blank} />
        <Stat label={t("contacts.report.mobiles")} value={report.mobiles} />
        <Stat label={t("contacts.report.fixedLines")} value={report.fixedLines} />
      </dl>

      {report.byReason.length === 0 ? null : (
        <div className="flex flex-col gap-1">
          <h3 className="text-sm font-medium">{t("contacts.report.byReason")}</h3>
          <ul className="text-sm text-[var(--shinobi-muted)]">
            {report.byReason.map((entry) => (
              <li key={entry.reason}>
                {t(`contacts.reason.${entry.reason}`, entry.reason)} — {entry.count}
              </li>
            ))}
          </ul>
        </div>
      )}

      {report.rejectedRows.length === 0 ? null : (
        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-3">
            <button
              type="button"
              onClick={download}
              className="rounded-md border border-[var(--shinobi-border)] px-3 py-2 text-sm hover:bg-[var(--shinobi-hover)]"
            >
              {t("contacts.report.export")}
            </button>
            {report.rejectedTruncated ? (
              <p className="text-xs text-[var(--shinobi-muted)]">
                {t("contacts.report.truncated")}
              </p>
            ) : null}
          </div>

          <div className="max-h-48 overflow-y-auto rounded-md border border-[var(--shinobi-border)]">
            <table className="w-full text-left text-sm">
              <caption className="sr-only">{t("contacts.report.rejectedCaption")}</caption>
              <thead className="sticky top-0 bg-[var(--shinobi-surface)]">
                <tr>
                  <th scope="col" className="px-3 py-2 font-medium">
                    {t("contacts.report.line")}
                  </th>
                  <th scope="col" className="px-3 py-2 font-medium">
                    {t("contacts.report.reason")}
                  </th>
                  <th scope="col" className="px-3 py-2 font-medium">
                    {t("contacts.report.value")}
                  </th>
                </tr>
              </thead>
              <tbody>
                {report.rejectedRows.slice(0, 200).map((row, index) => (
                  <tr
                    key={`${row.line}-${index}`}
                    className="border-t border-[var(--shinobi-border)]"
                  >
                    <td className="px-3 py-1">{row.line}</td>
                    <td className="px-3 py-1">{t(`contacts.reason.${row.reason}`, row.reason)}</td>
                    <td className="px-3 py-1 font-mono">{row.value}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </section>
  );
}

interface StatProps {
  readonly label: string;
  readonly value: number;
}

/** One figure of the report. */
function Stat({ label, value }: StatProps) {
  return (
    <div className="flex flex-col">
      <dt className="text-xs text-[var(--shinobi-muted)]">{label}</dt>
      <dd className="text-lg font-medium tabular-nums">{value}</dd>
    </div>
  );
}

/**
 * Quotes a CSV field.
 *
 * The value comes from the operator's own file and goes straight back into
 * one, so a value holding a comma, a quote or a newline has to survive the
 * round trip — otherwise the correction file is misaligned exactly on the rows
 * that were hardest to get right.
 */
function quote(value: string): string {
  return `"${value.replaceAll('"', '""')}"`;
}
