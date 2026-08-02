import { create } from "zustand";

import {
  contactsCancelImport,
  contactsImport,
  contactsLists,
  contactsPage,
  contactsProfiles,
  contactsSaveProfile,
  type ContactListDto,
  type ContactRowDto,
  type ImportOptionsInput,
  type ImportProfileDto,
  type ImportProgressEvent,
  type ImportReportDto,
  type ImportSourceInput,
  type SelectionInput,
} from "../ipc";

/**
 * State of the contacts screen (spec §13.4).
 *
 * # Pagination lives here, not in the component
 *
 * Same reason as the log screen: "ask for the next page" has to be idempotent
 * under React strict mode, which mounts every effect twice, and has to refuse
 * to run while a request is in flight or a fast scroll fires ten identical
 * requests. Both are `loading` and `cursor` below.
 *
 * # Rows accumulate, the selection replaces
 *
 * Scrolling **appends** a page. Changing the list selection or the search
 * **replaces** everything and resets the cursor: a filtered table still
 * showing the previous selection's rows would be showing contacts the operator
 * excluded.
 *
 * # Why the import does not go through `loading`
 *
 * An import runs for minutes and the table stays usable throughout — the two
 * have separate flags on purpose. `importing` also survives a `progress` of
 * `done`, because the report arrives on the promise and not on the event, and
 * a screen that cleared the bar on `done` would show nothing at all during the
 * final commit.
 */

/** Rows a page holds. Matches the backend's default. */
const PAGE = 100;

interface ContactsState {
  /** Contact rows loaded so far, in insertion order. */
  readonly rows: readonly ContactRowDto[];
  /** How many contacts the selection holds in total. */
  readonly total: number;
  /** Cursor of the next page, or `null` when the table is complete. */
  readonly cursor: string | null;
  /** Whether a page request is in flight. */
  readonly loading: boolean;
  /** Whether the last page request reached the end. */
  readonly complete: boolean;
  /**
   * Which query the rows belong to.
   *
   * Bumped by every `reload`. A response whose generation is no longer the
   * current one is **discarded**: without it, an operator typing "ab" can end
   * up looking at the results of "a", because the two requests race and the
   * slower one wins by arriving last.
   */
  readonly generation: number;

  /** Which lists the table spans. */
  readonly selection: SelectionInput;
  /** The search in force. */
  readonly search: string;

  /** Every contact list. */
  readonly lists: readonly ContactListDto[];
  /** Every saved mapping profile. */
  readonly profiles: readonly ImportProfileDto[];

  /** Whether an import is running. */
  readonly importing: boolean;
  /** The last progress reading, or `null` outside an import. */
  readonly progress: ImportProgressEvent | null;
  /** The report of the last finished import. */
  readonly report: ImportReportDto | null;

  /** Loads the first page, discarding what is held. */
  readonly reload: () => Promise<void>;
  /** Loads the page after the last one, if there is one. */
  readonly loadMore: () => Promise<void>;
  /** Replaces the list selection and reloads. */
  readonly setSelection: (selection: SelectionInput) => Promise<void>;
  /** Replaces the search and reloads. */
  readonly setSearch: (search: string) => Promise<void>;
  /** Loads the lists and the mapping profiles. */
  readonly loadReferences: () => Promise<void>;
  /** Runs an import and reloads the table when it ends. */
  readonly runImport: (source: ImportSourceInput, options: ImportOptionsInput) => Promise<void>;
  /** Asks the running import to stop. */
  readonly cancelImport: () => Promise<void>;
  /** Records a progress reading arriving from `import:progress`. */
  readonly applyProgress: (progress: ImportProgressEvent) => void;
  /** Saves a mapping profile and refreshes the list. */
  readonly saveProfile: (profile: ImportProfileDto) => Promise<void>;
  /** Clears the report panel. */
  readonly dismissReport: () => void;
}

/** The selection a screen starts on: every contact, no list restriction. */
const EVERYTHING: SelectionInput = {
  combination: "everything",
  lists: [],
  excluded: [],
};

export const useContacts = create<ContactsState>((set, get) => ({
  rows: [],
  total: 0,
  cursor: null,
  loading: false,
  complete: false,
  generation: 0,
  selection: EVERYTHING,
  search: "",
  lists: [],
  profiles: [],
  importing: false,
  progress: null,
  report: null,

  reload: async () => {
    // No `loading` guard here, deliberately. `reload` REPLACES, so a second
    // one is not a duplicate of the first — it supersedes it, and refusing to
    // issue it would leave the screen showing the previous query's rows with
    // no request in flight to correct them. The guard belongs on `loadMore`,
    // which appends. Staleness is handled by the generation instead.
    const generation = get().generation + 1;

    set({ loading: true, generation });

    const { selection, search } = get();
    const outcome = await contactsPage(selection, search || null, null, PAGE);

    if (get().generation !== generation) return;

    if (outcome.ok) {
      set({
        rows: outcome.value.rows,
        total: outcome.value.total,
        cursor: outcome.value.next,
        complete: outcome.value.next === null,
        loading: false,
      });
    } else {
      set({ loading: false });
    }
  },

  loadMore: async () => {
    const { loading, complete, cursor, selection, search } = get();

    // Three guards and each one is a distinct way the table breaks: a second
    // request while one is in flight duplicates rows, a request past the end
    // loops for ever, and a null cursor after a complete page would restart
    // from the top.
    if (loading || complete || cursor === null) return;

    set({ loading: true });

    const generation = get().generation;
    const outcome = await contactsPage(selection, search || null, cursor, PAGE);

    // A page that started under the previous query must not be appended to the
    // rows of the new one — that is how a table ends up mixing two selections.
    if (get().generation !== generation) return;

    if (outcome.ok) {
      set((state) => ({
        rows: [...state.rows, ...outcome.value.rows],
        total: outcome.value.total,
        cursor: outcome.value.next,
        complete: outcome.value.next === null,
        loading: false,
      }));
    } else {
      set({ loading: false });
    }
  },

  setSelection: async (selection) => {
    set({ selection, rows: [], cursor: null, complete: false });
    await get().reload();
  },

  setSearch: async (search) => {
    set({ search, rows: [], cursor: null, complete: false });
    await get().reload();
  },

  loadReferences: async () => {
    const [lists, profiles] = await Promise.all([contactsLists(), contactsProfiles()]);

    if (lists.ok) set({ lists: lists.value });
    if (profiles.ok) set({ profiles: profiles.value });
  },

  runImport: async (source, options) => {
    if (get().importing) return;

    set({ importing: true, progress: null, report: null });

    const outcome = await contactsImport(source, options);

    set({
      importing: false,
      progress: null,
      report: outcome.ok ? outcome.value : null,
    });

    // Reloads whatever the outcome: an import that failed halfway still wrote
    // the batches it committed, and a table that did not refresh would be
    // showing fewer contacts than the database holds.
    set({ rows: [], cursor: null, complete: false });
    await get().reload();
    await get().loadReferences();
  },

  cancelImport: async () => {
    await contactsCancelImport();
  },

  applyProgress: (progress) => {
    // Kept even when `done`: the report arrives on the promise, and clearing
    // the bar here would blank the panel during the final commit.
    set({ progress });
  },

  saveProfile: async (profile) => {
    const outcome = await contactsSaveProfile(profile);

    if (outcome.ok) {
      await get().loadReferences();
    }
  },

  dismissReport: () => set({ report: null }),
}));
