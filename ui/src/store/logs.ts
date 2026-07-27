import { create } from "zustand";

import {
  logsOrphans,
  logsPdus,
  logsQuery,
  logsSetPduLogging,
  type LogFilterInput,
  type LogRowDto,
  type OrphanRowDto,
  type PduRowDto,
} from "../ipc";

/**
 * State of the log screen (spec §13.3).
 *
 * # Pagination lives here, not in the component
 *
 * The table is virtualised, so the component renders a window and asks for
 * more when the window reaches the end of what it holds. That "ask for more"
 * has to be idempotent under React's strict mode — which mounts every effect
 * twice — and has to refuse to run while a request is in flight, or a fast
 * scroll fires ten identical page requests. Both are `loading` and `cursor`
 * below, and neither belongs in a component's render.
 *
 * # Rows accumulate, filters replace
 *
 * Scrolling **appends** a page. Changing a filter **replaces** everything and
 * resets the cursor: a filtered list that kept the rows of the previous filter
 * would be showing rows the operator excluded.
 */

/** Which of the three tables the screen shows. */
export type LogTab = "messages" | "orphans" | "pdus";

/** Rows a page holds. Matches the backend's default. */
const PAGE = 100;

interface LogsState {
  /** The table currently shown. */
  readonly tab: LogTab;
  /** The filter in force. */
  readonly filter: LogFilterInput;
  /** Message rows loaded so far, oldest first. */
  readonly rows: readonly LogRowDto[];
  /** How many rows the filter selects in total. */
  readonly total: number;
  /** Orphaned receipts loaded so far. */
  readonly orphans: readonly OrphanRowDto[];
  /** How many orphans there are in total. */
  readonly orphanTotal: number;
  /** Recorded PDUs loaded so far. */
  readonly pdus: readonly PduRowDto[];
  /** Whether PDU recording is on. */
  readonly pduLogging: boolean;
  /**
   * Cursor for the next page of the current tab, or `null` at the end.
   *
   * `null` is also the initial value, which is why {@link LogsState.exhausted}
   * exists: "no cursor yet" and "no more pages" are different states and a
   * single field cannot tell them apart.
   */
  readonly cursor: string | null;
  /** Whether the current tab has no further page. */
  readonly exhausted: boolean;
  /** Whether a request is in flight. */
  readonly loading: boolean;
  /** The last failure, for the screen to show. */
  readonly failure: string | null;
  /** The row whose detail panel is open, by identifier. */
  readonly selected: string | null;

  /** Switches table, resetting what the new one shows. */
  readonly show: (tab: LogTab) => void;
  /** Replaces the filter and reloads from the top. */
  readonly setFilter: (filter: LogFilterInput) => void;
  /** Loads the first page of the current tab, discarding what is loaded. */
  readonly reload: () => Promise<void>;
  /** Loads the next page, if there is one and none is in flight. */
  readonly loadMore: () => Promise<void>;
  /** Opens or closes a row's detail panel. */
  readonly select: (id: string | null) => void;
  /** Turns PDU recording on or off. */
  readonly setPduLogging: (enabled: boolean) => Promise<void>;
}

/** The empty filter: everything, unrestricted. */
export const NO_FILTER: LogFilterInput = {
  sessionId: null,
  campaignId: null,
  state: null,
  createdFrom: null,
  createdTo: null,
  destPrefix: null,
  dlrErr: null,
  search: null,
};

export const useLogs = create<LogsState>()((set, get) => {
  /** Loads one page, appending or replacing. */
  const fetchPage = async (append: boolean) => {
    const { tab, filter, cursor, loading } = get();

    if (loading) {
      return;
    }

    set({ loading: true, failure: null });

    const from = append ? cursor : null;

    if (tab === "messages") {
      const outcome = await logsQuery(filter, from, PAGE);

      if (!outcome.ok) {
        set({ loading: false, failure: describe(outcome.failure) });
        return;
      }

      set((state) => ({
        rows: append ? [...state.rows, ...outcome.value.rows] : outcome.value.rows,
        total: outcome.value.total,
        cursor: outcome.value.next,
        exhausted: outcome.value.next === null,
        loading: false,
      }));
      return;
    }

    if (tab === "orphans") {
      const outcome = await logsOrphans(filter.sessionId, from, PAGE);

      if (!outcome.ok) {
        set({ loading: false, failure: describe(outcome.failure) });
        return;
      }

      set((state) => ({
        orphans: append ? [...state.orphans, ...outcome.value.rows] : outcome.value.rows,
        orphanTotal: outcome.value.total,
        cursor: outcome.value.next,
        exhausted: outcome.value.next === null,
        loading: false,
      }));
      return;
    }

    const outcome = await logsPdus(filter.sessionId, from, PAGE);

    if (!outcome.ok) {
      set({ loading: false, failure: describe(outcome.failure) });
      return;
    }

    set((state) => ({
      pdus: append ? [...state.pdus, ...outcome.value.rows] : outcome.value.rows,
      pduLogging: outcome.value.enabled,
      cursor: outcome.value.next,
      exhausted: outcome.value.next === null,
      loading: false,
    }));
  };

  return {
    tab: "messages",
    filter: NO_FILTER,
    rows: [],
    total: 0,
    orphans: [],
    orphanTotal: 0,
    pdus: [],
    pduLogging: false,
    cursor: null,
    exhausted: false,
    loading: false,
    failure: null,
    selected: null,

    show: (tab) => {
      set({ tab, cursor: null, exhausted: false, selected: null });
      void get().reload();
    },

    setFilter: (filter) => {
      // The rows already loaded belong to the previous filter. Keeping them
      // while the new page arrives would show, for a moment, exactly the rows
      // the operator has just excluded.
      set({ filter, cursor: null, exhausted: false, rows: [], orphans: [], pdus: [] });
      void get().reload();
    },

    reload: async () => {
      set({ cursor: null, exhausted: false });
      await fetchPage(false);
    },

    loadMore: async () => {
      if (get().exhausted || get().cursor === null) {
        return;
      }

      await fetchPage(true);
    },

    select: (selected) => set({ selected }),

    setPduLogging: async (enabled) => {
      const outcome = await logsSetPduLogging(enabled);

      if (!outcome.ok) {
        set({ failure: describe(outcome.failure) });
        return;
      }

      // The value the backend reports, not the one requested: the switch shows
      // what is in force.
      set({ pduLogging: outcome.value });
      await get().reload();
    },
  };
});

/**
 * Renders a failure for the screen.
 *
 * A backend failure has a stable `code` the screen translates; a transport one
 * has none, because Rust never produced one, and minting a code here would be
 * hand-writing a piece of the contract ADR 0003 requires to be generated.
 */
function describe(failure: { kind: string; error?: { code: string }; message?: string }): string {
  return failure.kind === "backend" ? (failure.error?.code ?? "") : (failure.message ?? "");
}
