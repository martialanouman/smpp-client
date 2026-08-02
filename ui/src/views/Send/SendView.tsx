import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { onMessageUpdate } from "../../ipc";
import { usePreferences } from "../../store/preferences";
import { useSend } from "../../store/send";
import { useSessions } from "../../store/sessions";
import { CampaignView } from "./Campaign/CampaignView";
import { SendResult } from "./Simple/SendResult";
import { SimpleForm } from "./Simple/SimpleForm";

/** The two ways of sending (spec §10.1 and §10.2). */
const TABS = ["simple", "campaign"] as const;

/** One of them. */
type Tab = (typeof TABS)[number];

/**
 * The Send screen (spec §21).
 *
 * Two tabs since milestone 010: the unit form of §10.1 and the campaigns of
 * §10.2. The bar arrived with the second tab rather than before it — a bar with
 * one tab is furniture.
 *
 * # Why the campaign tab is unmounted rather than hidden
 *
 * Its effects subscribe to `campaign:progress` and load the campaign list, and
 * an operator writing a unit message has no use for either. Keeping it mounted
 * behind a `hidden` attribute would hold the subscription — four events a
 * second during a campaign — for a screen nobody is looking at.
 *
 * The cost is stated: switching back re-reads the list. That is one command,
 * and the live counters resume on the next reading, which is 250 ms away.
 *
 * Two effects below, and neither computes anything:
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
  const [tab, setTab] = useState<Tab>("simple");

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
      // Since milestone 008 the event carries a BATCH: the receipt pipeline
      // commits up to two hundred transitions at once, and one event each
      // would be what CA-008-08 forbids. A unit send still produces batches of
      // one, so this loop runs once on the path this screen cares about.
      for (const update of payload.updates) {
        adopt(update.clientMessageId, update.state);
      }
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
      <div role="tablist" aria-label={t("send.tabs.label")} className="mb-6 flex gap-1">
        {TABS.map((entry) => (
          <button
            key={entry}
            type="button"
            role="tab"
            id={`send-tab-${entry}`}
            aria-selected={tab === entry}
            aria-controls={`send-panel-${entry}`}
            onClick={() => setTab(entry)}
            className={`rounded-md px-3 py-1.5 text-sm ${
              tab === entry
                ? "bg-[var(--shinobi-hover)] font-medium"
                : "opacity-70 hover:opacity-100"
            }`}
          >
            {t(`send.tabs.${entry}`)}
          </button>
        ))}
      </div>

      {tab === "simple" ? (
        <div role="tabpanel" id="send-panel-simple" aria-labelledby="send-tab-simple">
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
        </div>
      ) : (
        <div role="tabpanel" id="send-panel-campaign" aria-labelledby="send-tab-campaign">
          <CampaignView />
        </div>
      )}
    </>
  );
}
