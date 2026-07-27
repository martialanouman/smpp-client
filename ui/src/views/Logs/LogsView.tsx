import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { onMessageUpdate } from "../../ipc";
import { NO_FILTER, useLogs, type LogTab } from "../../store/logs";
import { usePreferences } from "../../store/preferences";
import { LogFilters } from "./LogFilters";
import { LogDetail } from "./LogDetail";
import { useRowWindow } from "./rowWindow";

/**
 * The log screen (spec §13.3, EF-LOG-01).
 *
 * # Virtualised, and why that is not a nicety
 *
 * CA-008-07 asks for 200 000 rows with fluid scrolling. A `<tbody>` holding
 * 200 000 `<tr>` is 200 000 DOM nodes: the WebView spends seconds laying them
 * out and megabytes keeping them. {@link useRowWindow} renders the rows inside
 * the viewport and a small overscan — a few dozen nodes, whatever the total.
 * See `rowWindow.ts` for why that hook is thirty lines here instead of a
 * dependency.
 *
 * # The scrollbar is sized from what is LOADED, not from the total
 *
 * Which makes this an infinite scroll rather than a pre-sized list, and that is
 * worth being plain about: reaching row 200 000 means two thousand sequential
 * page requests. The count beside the tabs is the backend's total, so the
 * operator always knows how much there is — but the scrollbar tells them how
 * much they have.
 *
 * Sizing it from the total instead would need the pager to seek to an arbitrary
 * offset, and the cursor pagination this rests on deliberately cannot: a cursor
 * is a position in a result set, not an index into one (see
 * `persistence::Cursor`). Offset paging would give the scrollbar and take back
 * the constant per-page cost that makes 200 000 rows work at all. The filters
 * are the answer to "I need a row far down", and they are one query.
 *
 * The other half of the criterion is the backend's: the filter runs in SQLite
 * over an index, and the rows arrive one page at a time. Neither half works
 * alone — a virtualised table over a `SELECT *` still waits for the whole
 * table, and a paginated query rendered into 200 000 nodes still freezes.
 *
 * # The live badge
 *
 * `message:update` carries the aggregated increments of one commit
 * (CA-008-08). The screen does **not** try to patch its rows from them: a row
 * the operator has filtered out could arrive in a batch, and merging it would
 * show a row the filter excludes. It counts them, shows "N updates", and
 * reloads on demand. Deliberately less clever, and never wrong.
 */
export function LogsView() {
  const { t } = useTranslation();
  const notify = usePreferences((state) => state.notify);

  const tab = useLogs((state) => state.tab);
  const rows = useLogs((state) => state.rows);
  const orphans = useLogs((state) => state.orphans);
  const pdus = useLogs((state) => state.pdus);
  const total = useLogs((state) => state.total);
  const orphanTotal = useLogs((state) => state.orphanTotal);
  const loading = useLogs((state) => state.loading);
  const failure = useLogs((state) => state.failure);
  const exhausted = useLogs((state) => state.exhausted);
  const show = useLogs((state) => state.show);
  const reload = useLogs((state) => state.reload);
  const loadMore = useLogs((state) => state.loadMore);
  const select = useLogs((state) => state.select);

  // Transitions announced since the last read, as a COUNT rather than as rows.
  //
  // The screen deliberately does not merge them into its table: a message the
  // operator has filtered out can arrive in a batch, and merging it would show
  // a row the filter excludes. Counting is less clever and never wrong.
  const [pending, setPending] = useState(0);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    // Same teardown discipline as the send screen: without it a remount stacks
    // listeners, and without the `catch` a failed subscription would take the
    // whole screen down instead of only stopping the live badge.
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    onMessageUpdate((payload) => {
      setPending((count) => count + payload.updates.length);
    })
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
  }, [notify]);

  const items = useMemo(() => {
    if (tab === "messages") return rows;
    if (tab === "orphans") return orphans;
    return pdus;
  }, [tab, rows, orphans, pdus]);

  // A fixed row height: measuring every row would defeat the point, and the
  // table's cells are single-line by construction.
  const { ref: scroller, window: viewport, onScroll } = useRowWindow(items.length, ROW_HEIGHT);

  useEffect(() => {
    // Ask for the next page when the window reaches the end of what is loaded.
    // `loadMore` refuses to run while a request is in flight and when there is
    // no cursor left, so a fast scroll cannot fire ten identical requests.
    if (viewport.end >= items.length && items.length > 0 && !exhausted && !loading) {
      void loadMore();
    }
  }, [viewport.end, items.length, exhausted, loading, loadMore]);

  const shown = tab === "messages" ? total : tab === "orphans" ? orphanTotal : items.length;

  return (
    <div className="flex h-[calc(100vh-10rem)] flex-col gap-4">
      <div className="flex items-center gap-2">
        {(["messages", "orphans", "pdus"] as const).map((candidate) => (
          <TabButton key={candidate} tab={candidate} current={tab} onSelect={show} />
        ))}

        <span className="ml-auto text-sm text-[var(--shinobi-muted)]">
          {t("logs.count", { count: shown })}
        </span>

        {pending === 0 ? null : (
          <span
            aria-live="polite"
            className="rounded bg-[var(--shinobi-accent)] px-2 py-0.5 text-xs font-medium"
          >
            {t("logs.pending", { count: pending })}
          </span>
        )}

        <button
          type="button"
          onClick={() => {
            setPending(0);
            void reload();
          }}
          className="rounded-md border border-[var(--shinobi-border)] px-3 py-1.5 text-sm hover:bg-[var(--shinobi-hover)]"
        >
          {t("logs.refresh")}
        </button>
      </div>

      <LogFilters tab={tab} />

      {failure === null ? null : (
        <p role="alert" className="text-sm text-[var(--shinobi-danger)]">
          {t([`error.${failure}`, "logs.failure"])}
        </p>
      )}

      <div
        ref={scroller}
        // The window is derived from `scrollTop`, so it has to be recomputed as
        // the container scrolls. The hook ignores an event that moved nothing,
        // so this does not re-render on every pixel.
        onScroll={onScroll}
        className="flex-1 overflow-auto rounded-md border border-[var(--shinobi-border)]"
      >
        <table className="w-full border-collapse text-left text-sm">
          <thead className="sticky top-0 z-10 bg-[var(--shinobi-surface)]">
            <tr>
              {headers(tab).map((key) => (
                <th key={key} scope="col" className="px-3 py-2 font-medium">
                  {t(`logs.column.${key}`)}
                </th>
              ))}
            </tr>
          </thead>

          <tbody style={{ height: `${String(viewport.totalHeight)}px`, position: "relative" }}>
            {items.slice(viewport.start, viewport.end).map((row, offset) => (
              <tr
                key={rowKey(tab, row)}
                onClick={() => {
                  select(rowKey(tab, row));
                }}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  height: `${String(ROW_HEIGHT)}px`,
                  transform: `translateY(${String(viewport.offsetTop + offset * ROW_HEIGHT)}px)`,
                  display: "table",
                  tableLayout: "fixed",
                }}
                className="cursor-pointer border-t border-[var(--shinobi-border)] hover:bg-[var(--shinobi-hover)]"
              >
                {cells(tab, row).map((cell, index) => (
                  // The cells of one row are positional by definition:
                  // column three is column three, and there is no other
                  // identity a key could be built from.
                  <td key={index} className="truncate px-3 py-2" title={cell.title ?? cell.text}>
                    {cell.state === undefined ? (
                      cell.text
                    ) : (
                      <StateBadge state={cell.state} label={cell.text} />
                    )}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>

        {items.length === 0 && !loading ? (
          <p className="p-6 text-center text-sm text-[var(--shinobi-muted)]">{t("logs.empty")}</p>
        ) : null}
      </div>

      <LogDetail />
    </div>
  );
}

/** Height of one row, in pixels. Fixed: see `rowWindow.ts`. */
const ROW_HEIGHT = 36;

/** One tab of the screen. */
function TabButton({
  tab,
  current,
  onSelect,
}: {
  readonly tab: LogTab;
  readonly current: LogTab;
  readonly onSelect: (tab: LogTab) => void;
}) {
  const { t } = useTranslation();
  const active = tab === current;

  return (
    <button
      type="button"
      aria-current={active ? "page" : "false"}
      onClick={() => {
        onSelect(tab);
      }}
      className={[
        "rounded-md px-3 py-1.5 text-sm transition-colors",
        active
          ? "bg-[var(--shinobi-accent)] font-medium"
          : "border border-[var(--shinobi-border)] hover:bg-[var(--shinobi-hover)]",
      ].join(" ")}
    >
      {t(`logs.tab.${tab}`)}
    </button>
  );
}

/**
 * A state, colour-coded (spec §13.3).
 *
 * The colour is never the only signal: the code is written out beside it, so
 * the table stays readable to anyone who does not distinguish the two greens —
 * and in a screenshot printed in black and white.
 */
function StateBadge({ state, label }: { readonly state: string; readonly label: string }) {
  const tone =
    state === "DELIVERED"
      ? "bg-emerald-500/15 text-emerald-500"
      : state === "FAILED" || state === "EXPIRED"
        ? "bg-red-500/15 text-red-500"
        : state === "ACCEPTED"
          ? "bg-sky-500/15 text-sky-500"
          : "bg-[var(--shinobi-hover)]";

  return <span className={`rounded px-2 py-0.5 text-xs font-medium ${tone}`}>{label}</span>;
}

/** One rendered cell. */
interface Cell {
  readonly text: string;
  /** Set on the state column, so the badge knows which colour to take. */
  readonly state?: string;
  /** Shown on hover when the cell is truncated. */
  readonly title?: string;
}

/** The columns of one tab, as i18n keys. */
function headers(tab: LogTab): readonly string[] {
  if (tab === "messages") {
    return ["createdAt", "dest", "state", "status", "dlrStat", "dlrErr", "segments", "text"];
  }

  if (tab === "orphans") {
    return ["receivedAt", "smscId", "reason", "dlrStat", "dlrErr", "raw"];
  }

  return ["ts", "direction", "commandId", "commandStatus", "sequence"];
}

/** The stable identity of a row, and its React key. */
function rowKey(tab: LogTab, row: unknown): string {
  if (tab === "messages") {
    return (row as { clientMessageId: string }).clientMessageId;
  }

  return (row as { id: string }).id;
}

/** The cells of one row, in column order. */
function cells(tab: LogTab, row: unknown): readonly Cell[] {
  if (tab === "messages") {
    const message = row as Record<string, string | number | null>;

    return [
      { text: text(message.createdAt) },
      { text: text(message.destAddr) },
      { text: text(message.state), state: text(message.state) },
      { text: text(message.commandStatusSymbol) },
      { text: text(message.dlrStat) },
      { text: text(message.dlrErr) },
      { text: text(message.segments) },
      { text: text(message.text) },
    ];
  }

  if (tab === "orphans") {
    const orphan = row as Record<string, string | null>;

    return [
      { text: text(orphan.receivedAt) },
      { text: text(orphan.smscMessageId) },
      { text: text(orphan.reason) },
      { text: text(orphan.dlrStat) },
      { text: text(orphan.dlrErr) },
      { text: text(orphan.raw), title: text(orphan.raw) },
    ];
  }

  const pdu = row as Record<string, string | number | null>;

  return [
    { text: text(pdu.ts) },
    { text: text(pdu.direction) },
    { text: text(pdu.commandId) },
    { text: text(pdu.commandStatus) },
    { text: text(pdu.sequenceNumber) },
  ];
}

/** Renders a cell value, with an em dash for an absent one. */
function text(value: string | number | null | undefined): string {
  return value === null || value === undefined ? "—" : String(value);
}

export { NO_FILTER };
