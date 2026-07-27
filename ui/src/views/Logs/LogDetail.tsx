import { useTranslation } from "react-i18next";

import { useLogs } from "../../store/logs";

/**
 * The detail panel opened by clicking a row (spec §13.3, CA-008-09).
 *
 * # What it shows for a PDU, and what it shows when there is nothing
 *
 * On the PDU tab it shows the four header fields, the decoded body with its
 * TLVs, and the raw hexadecimal — the four things CA-008-09 lists. All three of
 * the last are `null` unless recording was on when the PDU crossed the socket,
 * because the recorder produces nothing at all while it is off (CLAUDE.md §8).
 *
 * So the panel says **why** it is empty. "Nothing recorded" and "recording is
 * off" look identical on screen and are completely different situations, and a
 * screen that showed the same emptiness for both would send an operator
 * hunting a bug that is a switch.
 */
export function LogDetail() {
  const { t } = useTranslation();
  const selected = useLogs((state) => state.selected);
  const tab = useLogs((state) => state.tab);
  const rows = useLogs((state) => state.rows);
  const orphans = useLogs((state) => state.orphans);
  const pdus = useLogs((state) => state.pdus);
  const pduLogging = useLogs((state) => state.pduLogging);
  const setPduLogging = useLogs((state) => state.setPduLogging);
  const select = useLogs((state) => state.select);

  const entries = detailOf(tab, selected, { rows, orphans, pdus });

  return (
    <aside
      aria-label={t("logs.detail.label")}
      className="rounded-md border border-[var(--shinobi-border)] p-3"
    >
      <div className="flex items-center gap-3">
        <h2 className="text-sm font-medium">{t("logs.detail.title")}</h2>

        {tab === "pdus" ? (
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={pduLogging}
              onChange={(event) => {
                void setPduLogging(event.target.checked);
              }}
            />
            {t("logs.detail.pduLogging")}
          </label>
        ) : null}

        {selected === null ? null : (
          <button
            type="button"
            onClick={() => {
              select(null);
            }}
            className="ml-auto rounded-md border border-[var(--shinobi-border)] px-2 py-1 text-xs hover:bg-[var(--shinobi-hover)]"
          >
            {t("logs.detail.close")}
          </button>
        )}
      </div>

      {tab === "pdus" && !pduLogging ? (
        <p className="mt-2 text-sm text-[var(--shinobi-muted)]">{t("logs.detail.pduDisabled")}</p>
      ) : null}

      {entries === null ? (
        <p className="mt-2 text-sm text-[var(--shinobi-muted)]">{t("logs.detail.none")}</p>
      ) : (
        <dl className="mt-2 grid grid-cols-[10rem_1fr] gap-x-4 gap-y-1 text-sm">
          {entries.map(([key, value]) => (
            <div key={key} className="contents">
              <dt className="text-[var(--shinobi-muted)]">{t(`logs.field.${key}`)}</dt>
              <dd className="break-all font-mono text-xs">
                {value === null || value === "" ? "—" : value}
              </dd>
            </div>
          ))}
        </dl>
      )}
    </aside>
  );
}

/** The fields of the selected row, in display order. */
function detailOf(
  tab: string,
  selected: string | null,
  data: {
    readonly rows: readonly Record<string, unknown>[];
    readonly orphans: readonly Record<string, unknown>[];
    readonly pdus: readonly Record<string, unknown>[];
  },
): readonly (readonly [string, string | null])[] | null {
  if (selected === null) {
    return null;
  }

  if (tab === "messages") {
    const row = data.rows.find((candidate) => candidate.clientMessageId === selected);

    return row === undefined
      ? null
      : ([
          ["clientMessageId", render(row.clientMessageId)],
          ["smscMessageId", render(row.smscMessageId)],
          ["sessionId", render(row.sessionId)],
          ["source", render(row.sourceAddr)],
          ["dest", render(row.destAddr)],
          ["state", render(row.state)],
          ["commandStatus", render(row.commandStatusSymbol)],
          ["dlrStat", render(row.dlrStat)],
          ["dlrErr", render(row.dlrErr)],
          ["segments", render(row.segments)],
          ["attempts", render(row.attempts)],
          ["createdAt", render(row.createdAt)],
          ["sentAt", render(row.sentAt)],
          ["respAt", render(row.respAt)],
          ["dlrAt", render(row.dlrAt)],
          ["text", render(row.text)],
        ] as const);
  }

  if (tab === "orphans") {
    const row = data.orphans.find((candidate) => candidate.id === selected);

    return row === undefined
      ? null
      : ([
          ["smscMessageId", render(row.smscMessageId)],
          ["reason", render(row.reason)],
          ["sessionId", render(row.sessionId)],
          ["dlrStat", render(row.dlrStat)],
          ["dlrErr", render(row.dlrErr)],
          ["receivedAt", render(row.receivedAt)],
          ["raw", render(row.raw)],
        ] as const);
  }

  const row = data.pdus.find((candidate) => candidate.id === selected);

  return row === undefined
    ? null
    : ([
        ["direction", render(row.direction)],
        ["commandId", render(row.commandId)],
        ["commandStatus", render(row.commandStatus)],
        ["sequence", render(row.sequenceNumber)],
        ["ts", render(row.ts)],
        ["decoded", render(row.decoded)],
        ["rawHex", render(row.rawHex)],
      ] as const);
}

/** Renders one field value. */
function render(value: unknown): string | null {
  return value === null || value === undefined ? null : String(value);
}
