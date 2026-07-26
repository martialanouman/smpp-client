import { create } from "zustand";

import type { IpcFailure, SessionProfileDto, SessionStatusDto } from "../ipc";
import {
  sessionBind,
  sessionCreate,
  sessionDelete,
  sessionList,
  sessionUnbind,
  sessionUpdate,
} from "../ipc";
import { usePreferences } from "./preferences";

/**
 * The profiles and the live state of their sessions.
 *
 * Two things this store deliberately does **not** hold.
 *
 * **The password.** It is read from the form, handed to `sessionBind`, and
 * forgotten. Keeping it "so the user does not have to retype it on a rebind"
 * would put a credential in the WebView's memory for the life of the process,
 * which is exactly what step-005 §2 and CLAUDE.md §8 rule out until milestone
 * 015 provides encryption at rest.
 *
 * **A derived state.** The status of a session comes from `sessions:state` and
 * from nowhere else. Computing "probably bound by now" locally is how an
 * interface ends up disagreeing with the backend it is meant to display.
 */
interface SessionsState {
  /** Every profile, oldest first. */
  readonly profiles: readonly SessionProfileDto[];
  /** Live state, keyed by `sessionId`. Absent means `CLOSED`. */
  readonly statuses: Readonly<Record<string, SessionStatusDto>>;
  /** Whether a backend call is in flight. */
  readonly busy: boolean;
  /** Reloads the profiles. */
  readonly refresh: () => Promise<void>;
  /** Creates or updates a profile. */
  readonly save: (profile: SessionProfileDto) => Promise<boolean>;
  /** Deletes a profile. */
  readonly remove: (sessionId: string) => Promise<void>;
  /** Opens a session. */
  readonly bind: (sessionId: string, password: string) => Promise<void>;
  /** Closes a session. */
  readonly unbind: (sessionId: string) => Promise<void>;
  /** Adopts a `sessions:state` payload. */
  readonly adopt: (statuses: readonly SessionStatusDto[]) => void;
}

/**
 * Turns a failure into a notification.
 *
 * The same funnel `bridge.ts` uses for preferences: a `backend` failure has a
 * translatable code, a `transport` one has none and says so.
 */
function notifyFailure(failure: IpcFailure): void {
  const { notify } = usePreferences.getState();

  if (failure.kind === "backend") {
    notify({ code: failure.error.code, message: failure.error.message });
  } else {
    notify({ code: null, message: failure.message });
  }
}

export const useSessions = create<SessionsState>((set, get) => ({
  profiles: [],
  statuses: {},
  busy: false,

  refresh: async () => {
    set({ busy: true });
    const outcome = await sessionList();
    set({ busy: false });

    if (outcome.ok) {
      set({ profiles: outcome.value });
    } else {
      notifyFailure(outcome.failure);
    }
  },

  save: async (profile) => {
    set({ busy: true });
    const outcome = profile.sessionId ? await sessionUpdate(profile) : await sessionCreate(profile);
    set({ busy: false });

    if (!outcome.ok) {
      notifyFailure(outcome.failure);

      return false;
    }

    await get().refresh();

    return true;
  },

  remove: async (sessionId) => {
    set({ busy: true });
    const outcome = await sessionDelete(sessionId);
    set({ busy: false });

    if (outcome.ok) {
      await get().refresh();
    } else {
      notifyFailure(outcome.failure);
    }
  },

  bind: async (sessionId, password) => {
    set({ busy: true });
    const outcome = await sessionBind({ sessionId, password });
    set({ busy: false });

    if (outcome.ok) {
      // The event will follow; adopting the returned status straight away is
      // what makes the button feel answered rather than ignored.
      get().adopt([outcome.value]);
    } else {
      notifyFailure(outcome.failure);
    }
  },

  unbind: async (sessionId) => {
    set({ busy: true });
    const outcome = await sessionUnbind(sessionId);
    set({ busy: false });

    if (!outcome.ok) {
      notifyFailure(outcome.failure);
    }
  },

  adopt: (statuses) => {
    set((state) => {
      const next: Record<string, SessionStatusDto> = { ...state.statuses };

      for (const status of statuses) {
        next[status.sessionId] = status;
      }

      return { statuses: next };
    });
  },
}));

/**
 * The defaults of spec §8.2, for a profile the user is about to create.
 *
 * Written out rather than left to the backend so the form has something to
 * show: the backend applies the same defaults, and a test holds the two
 * together.
 */
export function blankProfile(): SessionProfileDto {
  return {
    sessionId: null,
    name: "",
    host: "",
    port: 2775,
    bindType: "transceiver",
    interfaceVersion: "v5.0",
    systemId: "",
    systemType: "",
    windowSize: 50,
    throughputTps: 100,
    minTps: 1,
    enquireLinkS: 30,
    responseTimeoutS: 10,
    reconnectEnabled: true,
    minBackoffS: 1,
    maxBackoffS: 60,
    jitter: true,
    gsm7Packing: "unpacked",
    gsm7Charset: "gsm0338",
    bindCount: 1,
  };
}
