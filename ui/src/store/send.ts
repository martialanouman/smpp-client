import { create } from "zustand";

import type { IpcFailure, MessagePreviewDto, MessageSendInput, MessageSendResultDto } from "../ipc";
import { messagePreview, messageSend } from "../ipc";
import { usePreferences } from "./preferences";

/**
 * The Send › Simple form, its live counter and its last result.
 *
 * Three things this store deliberately does **not** do.
 *
 * **It does not count characters.** The counter comes from `message_preview`
 * and from nowhere else. A local `text.length` would be wrong on the first
 * `€` — two septets, not one — and would then disagree with the segments the
 * backend actually sends, which is what CA-006-09 is about.
 *
 * **It does not decide what is valid.** The form marks required fields and
 * stops there; every rule is applied by the backend, which treats this side as
 * untrusted (CLAUDE.md §3). A rejection comes back as a code the interface
 * translates.
 *
 * **It does not derive a state.** `QUEUED → SENT → ACCEPTED` arrives on
 * `message:update`. Guessing "probably sent by now" locally is how an
 * interface ends up disagreeing with the backend it is meant to display.
 */

/** The form, in the shape the command expects. */
export type SendForm = Omit<MessageSendInput, "sessionId">;

/** An empty form with the safe defaults of spec §23.3. */
export function blankForm(): SendForm {
  return {
    destination: "",
    destTon: "international",
    destNpi: "isdn",
    source: null,
    sourceTon: null,
    sourceNpi: null,
    text: "",
    encoding: "automatic",
    segmentationMode: "udh",
    serviceType: "",
    protocolId: 0,
    priorityFlag: 0,
    scheduleDeliveryTime: "",
    validityPeriod: "",
    registeredDelivery: "onAnyOutcome",
    replaceIfPresent: false,
    smDefaultMsgId: 0,
    tlvs: [],
  };
}

interface SendState {
  /** The session the message will go out on, or `""` before one is chosen. */
  readonly sessionId: string;
  /** The form as it stands. */
  readonly form: SendForm;
  /** The counter, or `null` before the first keystroke. */
  readonly preview: MessagePreviewDto | null;
  /** The last send, or `null`. */
  readonly result: MessageSendResultDto | null;
  /** The live state of the message being sent, from `message:update`. */
  readonly progress: string | null;
  /** Whether a send is in flight. */
  readonly sending: boolean;
  /** Chooses the session. */
  readonly chooseSession: (sessionId: string) => void;
  /** Replaces the form. */
  readonly update: (form: SendForm) => void;
  /** Recomputes the counter for the current form. */
  readonly refreshPreview: () => Promise<void>;
  /** Sends the message. */
  readonly send: () => Promise<void>;
  /** Adopts a `message:update` payload. */
  readonly adopt: (clientMessageId: string, state: string) => void;
}

/**
 * Turns a failure into a notification.
 *
 * The same funnel the other stores use: a `backend` failure has a translatable
 * code, a `transport` one has none and says so.
 */
function notifyFailure(failure: IpcFailure): void {
  const { notify } = usePreferences.getState();

  if (failure.kind === "backend") {
    notify({ code: failure.error.code, message: failure.error.message });
  } else {
    notify({ code: null, message: failure.message });
  }
}

export const useSend = create<SendState>((set, get) => ({
  sessionId: "",
  form: blankForm(),
  preview: null,
  result: null,
  progress: null,
  sending: false,

  chooseSession: (sessionId) => {
    set({ sessionId });
    // The GSM conventions belong to the session (ADR 0008, ADR 0009), so the
    // counter changes when the session does.
    void get().refreshPreview();
  },

  update: (form) => {
    set({ form });
  },

  refreshPreview: async () => {
    const { form, sessionId } = get();

    const outcome = await messagePreview({
      text: form.text,
      encoding: form.encoding,
      segmentationMode: form.segmentationMode,
      sessionId: sessionId === "" ? null : sessionId,
    });

    if (outcome.ok) {
      set({ preview: outcome.value });
    } else {
      // A preview failure is shown on the counter, not as a toast: a forced
      // encoding that cannot write the text is something the operator is in
      // the middle of typing, not an incident.
      set({ preview: null });
    }
  },

  send: async () => {
    const { form, sessionId } = get();

    set({ sending: true, result: null, progress: null });
    const outcome = await messageSend({ ...form, sessionId });
    set({ sending: false });

    if (outcome.ok) {
      set({ result: outcome.value, progress: outcome.value.state });
    } else {
      notifyFailure(outcome.failure);
    }
  },

  adopt: (clientMessageId, state) => {
    const { result } = get();

    // Only the message currently on screen: a `message:update` for another
    // one — a campaign at milestone 010 — must not overwrite this panel.
    if (result === null || result.clientMessageId === clientMessageId) {
      set({ progress: state });
    }
  },
}));
