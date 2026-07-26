import { useEffect } from "react";
import { useTranslation } from "react-i18next";

import { onMessageUpdate } from "../../ipc";
import { usePreferences } from "../../store/preferences";
import { useSend } from "../../store/send";
import { useSessions } from "../../store/sessions";
import { SendResult } from "./Simple/SendResult";
import { SimpleForm } from "./Simple/SimpleForm";

/**
 * The Send screen (spec §21).
 *
 * One tab at this milestone. Bulk, templates and scheduling arrive at
 * milestone 010 and the tab bar with them; a bar with a single tab today would
 * be furniture.
 *
 * Two effects, and neither computes anything:
 *
 * * the counter is recomputed by the **backend** whenever the text, the
 *   encoding or the mode changes — CA-006-09 wants the counter and the
 *   segmentation to agree, and the only way to guarantee that is for both to
 *   come from the same function;
 * * `message:update` drives the lifecycle badge, so `QUEUED → SENT →
 *   ACCEPTED` is what the backend reported rather than what the interface
 *   guessed.
 */
/**
 * How long to wait after the last keystroke before recomputing the counter.
 *
 * Below the ~100 ms at which a delay stops feeling instantaneous, and well
 * above the interval between two keystrokes of a fast typist.
 */
const PREVIEW_DEBOUNCE_MS = 60;

export function SendView() {
  const { t } = useTranslation();

  const profiles = useSessions((state) => state.profiles);
  const statuses = useSessions((state) => state.statuses);

  const sessionId = useSend((state) => state.sessionId);
  const form = useSend((state) => state.form);
  const preview = useSend((state) => state.preview);
  const result = useSend((state) => state.result);
  const progress = useSend((state) => state.progress);
  const sending = useSend((state) => state.sending);
  const chooseSession = useSend((state) => state.chooseSession);
  const update = useSend((state) => state.update);
  const refreshPreview = useSend((state) => state.refreshPreview);
  const send = useSend((state) => state.send);
  const adopt = useSend((state) => state.adopt);
  const notify = usePreferences((state) => state.notify);

  // Only the three inputs the counter depends on. Listing the whole form would
  // recompute it on every keystroke in `validity_period`, which changes
  // nothing it shows.
  //
  // Debounced, because the counter is a backend round trip and a typist
  // produces keystrokes faster than the bridge answers. The store drops
  // out-of-order answers on its own; this only stops the flood that makes them
  // likely. Short enough that the counter still feels immediate.
  useEffect(() => {
    const pending = setTimeout(() => {
      void refreshPreview();
    }, PREVIEW_DEBOUNCE_MS);

    return () => {
      clearTimeout(pending);
    };
  }, [refreshPreview, form.text, form.encoding, form.segmentationMode]);

  useEffect(() => {
    // Same shape as the `sessions:state` subscription, and for the same two
    // reasons: without the teardown a remount stacks listeners and every
    // update is adopted as many times as the screen was opened, and without
    // the `catch` a failed subscription takes the whole screen down instead
    // of only stopping the live badge.
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    onMessageUpdate((payload) => {
      adopt(payload.clientMessageId, payload.state);
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
  }, [adopt, notify]);

  return (
    <>
      <p className="mb-6 max-w-3xl text-sm opacity-70">{t("send.intro")}</p>

      <SimpleForm
        profiles={profiles}
        statuses={statuses}
        sessionId={sessionId}
        form={form}
        preview={preview}
        sending={sending}
        onSession={chooseSession}
        onChange={update}
        onSubmit={() => void send()}
      />

      <SendResult result={result} progress={progress} />
    </>
  );
}
