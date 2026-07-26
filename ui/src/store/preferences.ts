import { create } from "zustand";

import type { ErrorCode, Language, Theme } from "../ipc";

/**
 * The eight screens of spec §21, in navigation order.
 *
 * Declared `as const` so the union below is derived from this array rather
 * than written twice: adding a screen here is enough, and forgetting to add it
 * to the navigation becomes a type error instead of a missing menu entry.
 */
export const SCREENS = [
  "dashboard",
  "sessions",
  "send",
  "contacts",
  "numbers",
  "logs",
  "stats",
  "settings",
] as const;

/** One of the eight screens. */
export type Screen = (typeof SCREENS)[number];

/**
 * An error waiting to be shown.
 *
 * Carries an `id` rather than being keyed by position: React needs a stable
 * key, and two identical errors must not collapse into one toast.
 *
 * `code` is **nullable**, and that is the whole point. A backend error has a
 * stable `ErrorCode` the interface can translate and act on. A transport
 * failure — no backend, a serialisation mismatch — has none, because Rust
 * never produced one. Minting a code here would be hand-writing a piece of the
 * contract that ADR 0003 requires to be generated, and it would lie to whoever
 * later searched for it.
 */
export interface Notification {
  /** Distinct within a session. */
  readonly id: string;
  /** Stable identifier, or `null` for a transport failure. */
  readonly code: ErrorCode | null;
  /** Sentence to display. */
  readonly message: string;
}

/** What {@link PreferencesState.notify} accepts. */
export type NotificationInput = Pick<Notification, "code" | "message">;

/**
 * How many notifications are kept at once.
 *
 * A backend stuck in a failing loop would otherwise grow this array without
 * bound — the WebView would slow down long before anyone read the hundredth
 * toast.
 */
const MAX_NOTIFICATIONS = 5;

interface PreferencesState {
  /** The screen currently displayed. */
  readonly screen: Screen;
  /** Interface language. French is the default (guide §10.2). */
  readonly language: Language;
  /** Colour scheme, `system` meaning "follow the OS". */
  readonly theme: Theme;
  /** Pending error notifications, oldest first. */
  readonly notifications: readonly Notification[];

  /** Displays another screen. */
  readonly goTo: (screen: Screen) => void;
  /** Switches the interface language. */
  readonly setLanguage: (language: Language) => void;
  /** Switches the colour scheme. */
  readonly setTheme: (theme: Theme) => void;
  /** Queues an error for display. */
  readonly notify: (input: NotificationInput) => void;
  /** Removes one notification, by id. */
  readonly dismiss: (id: string) => void;
}

/**
 * Counter that makes ids distinct.
 *
 * `Date.now()` alone is not enough: two errors emitted in the same
 * millisecond — which is exactly what a failing loop produces — would share an
 * id, and React would treat the two toasts as one.
 */
let sequence = 0;

/**
 * Application preferences and navigation.
 *
 * Deliberately holds no router. Eight screens in a desktop application need
 * neither URLs nor history, and a routing dependency would buy deep-linking
 * that a WebView with no address bar cannot use. Navigation is state, and the
 * type of that state is checked.
 *
 * Persistence lives in the backend (`config_get` / `config_set`): the store is
 * the in-memory mirror, not the source of truth.
 */
export const usePreferences = create<PreferencesState>()((set) => ({
  screen: "dashboard",
  language: "fr",
  theme: "system",
  notifications: [],

  goTo: (screen) => set({ screen }),
  setLanguage: (language) => set({ language }),
  setTheme: (theme) => set({ theme }),

  notify: ({ code, message }) =>
    set((state) => {
      sequence += 1;
      const entry: Notification = { id: `${Date.now()}-${sequence}`, code, message };

      // Drops from the front: the newest error is the one the user just
      // caused, so it is the one that must survive.
      return {
        notifications: [...state.notifications, entry].slice(-MAX_NOTIFICATIONS),
      };
    }),

  dismiss: (id) =>
    set((state) => ({
      notifications: state.notifications.filter((entry) => entry.id !== id),
    })),
}));
