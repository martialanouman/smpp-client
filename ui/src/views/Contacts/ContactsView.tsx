import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { onImportProgress } from "../../ipc";
import { useContacts } from "../../store/contacts";
import { usePreferences } from "../../store/preferences";
import { ImportWizard } from "./ImportWizard";
import { ImportReport } from "./ImportReport";

/**
 * The contacts screen (EF-CNT-01, deliverable L-009-08).
 *
 * Three parts, and the split is the screen's argument: the wizard on top
 * because an empty table is what an operator arrives at, the report under it
 * because it is what they read after an import, and the table below because it
 * is what they come back to afterwards.
 *
 * # Virtualisation
 *
 * The table renders a window and asks the store for another page when the
 * window nears the end of what is loaded. The scroll handler is the only place
 * that knows about pixels; everything else counts rows.
 */
export function ContactsView() {
  const { t } = useTranslation();

  const rows = useContacts((state) => state.rows);
  const total = useContacts((state) => state.total);
  const loading = useContacts((state) => state.loading);
  const complete = useContacts((state) => state.complete);
  const search = useContacts((state) => state.search);
  const report = useContacts((state) => state.report);
  const reload = useContacts((state) => state.reload);
  const loadMore = useContacts((state) => state.loadMore);
  const setSearch = useContacts((state) => state.setSearch);
  const setSelection = useContacts((state) => state.setSelection);
  const lists = useContacts((state) => state.lists);
  const selection = useContacts((state) => state.selection);
  const loadReferences = useContacts((state) => state.loadReferences);
  const applyProgress = useContacts((state) => state.applyProgress);
  const notify = usePreferences((state) => state.notify);

  useEffect(() => {
    void reload();
    void loadReferences();
  }, [reload, loadReferences]);

  useEffect(() => {
    // The unlisten function arrives asynchronously, and the component can
    // unmount before it does. Without the flag, a fast unmount leaks a
    // listener that keeps firing on a dead store.
    let live = true;
    let unlisten: (() => void) | undefined;

    onImportProgress(applyProgress)
      .then((stop) => {
        if (live) {
          unlisten = stop;
        } else {
          stop();
        }
      })
      // Subscribing rejects whenever the Tauri API is unavailable — opening
      // the dev server in a plain browser is enough — and an unhandled
      // rejection here would take the screen down over a progress bar. The
      // table, the import and the report all work without this listener; the
      // bar simply stays at "starting". Same reasoning as `startBackendBridge`.
      .catch(() => {
        notify({
          code: null,
          message: "import progress events are unavailable",
        });
      });

    return () => {
      live = false;
      unlisten?.();
    };
  }, [applyProgress, notify]);

  return (
    <div className="flex flex-col gap-6">
      <ImportWizard />

      {report === null ? null : <ImportReport report={report} />}

      <section aria-labelledby="contacts-table-heading" className="flex flex-col gap-3">
        <div className="flex items-baseline justify-between gap-4">
          <h2 id="contacts-table-heading" className="text-lg font-medium">
            {t("contacts.table.heading")}
          </h2>
          <p className="text-sm text-[var(--shinobi-muted)]">
            {t("contacts.table.count", { count: total })}
          </p>
        </div>

        <div className="grid gap-3 sm:grid-cols-2">
          <label className="flex flex-col gap-1 text-sm">
            <span>{t("contacts.table.search")}</span>
            <input
              type="search"
              value={search}
              onChange={(event) => void setSearch(event.target.value)}
              placeholder={t("contacts.table.searchPlaceholder")}
              className="rounded-md border border-[var(--shinobi-border)] bg-transparent px-3 py-2"
            />
          </label>

          <label className="flex flex-col gap-1 text-sm">
            <span>{t("contacts.table.list")}</span>
            <select
              value={selection.lists?.[0] ?? ""}
              onChange={(event) =>
                void setSelection(
                  // An empty choice is "everything", not "the union of no
                  // list" — the latter selects nothing, which would blank the
                  // table the moment the operator cleared the filter.
                  event.target.value === ""
                    ? { combination: "everything", lists: [], excluded: [] }
                    : { combination: "union", lists: [event.target.value], excluded: [] },
                )
              }
              className="rounded-md border border-[var(--shinobi-border)] bg-transparent px-3 py-2"
            >
              <option value="">{t("contacts.table.allLists")}</option>
              {lists.map((list) => (
                <option key={list.listId} value={list.listId}>
                  {list.name}
                </option>
              ))}
            </select>
          </label>
        </div>

        <ContactTable
          rows={rows}
          loading={loading}
          complete={complete}
          onReachEnd={() => void loadMore()}
        />
      </section>
    </div>
  );
}

/** How close to the bottom, in pixels, triggers the next page. */
const LOAD_MORE_MARGIN = 240;

/** Height of one row, in pixels. Must match the `h-9` below. */
const ROW_HEIGHT = 36;

/** Rows rendered beyond the viewport, above and below. */
const OVERSCAN = 8;

interface ContactTableProps {
  readonly rows: readonly import("../../ipc").ContactRowDto[];
  readonly loading: boolean;
  readonly complete: boolean;
  readonly onReachEnd: () => void;
}

/**
 * The virtualised table.
 *
 * Renders a window of rows and pads it with two spacers, so the scrollbar is
 * sized by the whole set while the DOM holds a few dozen nodes. Two hundred
 * thousand rows in the DOM is what CLAUDE.md §4 forbids, and what freezes a
 * WebView.
 */
function ContactTable({ rows, loading, complete, onReachEnd }: ContactTableProps) {
  const { t } = useTranslation();
  const viewport = useRef<HTMLDivElement>(null);
  const [window, setWindow] = useState({ first: 0, last: 40 });

  const onScroll = () => {
    const element = viewport.current;
    if (element === null) return;

    const first = Math.max(0, Math.floor(element.scrollTop / ROW_HEIGHT) - OVERSCAN);
    const visible = Math.ceil(element.clientHeight / ROW_HEIGHT) + OVERSCAN * 2;

    setWindow({ first, last: first + visible });

    if (element.scrollHeight - element.scrollTop - element.clientHeight < LOAD_MORE_MARGIN) {
      onReachEnd();
    }
  };

  const shown = rows.slice(window.first, window.last);
  const above = window.first * ROW_HEIGHT;
  const below = Math.max(0, (rows.length - window.last) * ROW_HEIGHT);

  return (
    <div
      ref={viewport}
      onScroll={onScroll}
      className="max-h-[28rem] overflow-y-auto rounded-md border border-[var(--shinobi-border)]"
    >
      <table className="w-full text-left text-sm">
        <caption className="sr-only">{t("contacts.table.caption")}</caption>
        <thead className="sticky top-0 bg-[var(--shinobi-surface)]">
          <tr>
            <th scope="col" className="px-3 py-2 font-medium">
              {t("contacts.table.msisdn")}
            </th>
            <th scope="col" className="px-3 py-2 font-medium">
              {t("contacts.table.country")}
            </th>
            <th scope="col" className="px-3 py-2 font-medium">
              {t("contacts.table.lineType")}
            </th>
            <th scope="col" className="px-3 py-2 font-medium">
              {t("contacts.table.source")}
            </th>
            <th scope="col" className="px-3 py-2 font-medium">
              {t("contacts.table.createdAt")}
            </th>
          </tr>
        </thead>
        <tbody>
          {above > 0 ? (
            <tr aria-hidden="true">
              <td colSpan={5} style={{ height: above }} />
            </tr>
          ) : null}

          {shown.map((row) => (
            <tr key={row.contactId} className="h-9 border-t border-[var(--shinobi-border)]">
              <td className="px-3 font-mono">{row.msisdn}</td>
              <td className="px-3">{row.country ?? "—"}</td>
              <td className="px-3">
                {row.lineType === null ? "—" : t(`contacts.lineType.${row.lineType}`, row.lineType)}
              </td>
              <td className="px-3">
                {row.source === null ? "—" : t(`contacts.source.${row.source}`, row.source)}
              </td>
              <td className="px-3">{row.createdAt}</td>
            </tr>
          ))}

          {below > 0 ? (
            <tr aria-hidden="true">
              <td colSpan={5} style={{ height: below }} />
            </tr>
          ) : null}

          {rows.length === 0 && !loading ? (
            <tr>
              <td colSpan={5} className="px-3 py-6 text-center text-[var(--shinobi-muted)]">
                {t("contacts.table.empty")}
              </td>
            </tr>
          ) : null}
        </tbody>
      </table>

      <p aria-live="polite" className="px-3 py-2 text-xs text-[var(--shinobi-muted)]">
        {loading
          ? t("contacts.table.loading")
          : complete && rows.length > 0
            ? t("contacts.table.complete")
            : ""}
      </p>
    </div>
  );
}
