import { configGet, configSet, onErrorNotify } from "../ipc";
import type { AppConfig, ConfigSetInput, IpcOutcome } from "../ipc";
import { usePreferences } from "./preferences";

/**
 * Applies a backend config to the store.
 *
 * The backend is the source of truth: what it returns is adopted, not what was
 * submitted. It may normalise a value, and it may refuse one — in both cases
 * the interface must show what is actually stored.
 */
function adopt(config: AppConfig): void {
  usePreferences.getState().adoptConfig(config);
}

/**
 * Turns a failed call into a notification.
 *
 * A transport failure carries `null` as its code, deliberately: Rust never
 * produced one, and minting one here would be hand-writing a piece of the
 * contract ADR 0003 requires to be generated.
 */
/**
 * Surfaces a failed call as a notification.
 *
 * Exported because every store needs it and each one writing its own is how a
 * screen ends up swallowing its failures: the operator clicks, nothing
 * happens, and nothing says why. The `code` is what the toast translates; the
 * English `message` is for the logs and never reaches the screen.
 */
export function report(failure: Extract<IpcOutcome<never>, { ok: false }>["failure"]): void {
  usePreferences
    .getState()
    .notify(
      failure.kind === "backend"
        ? { code: failure.error.code, message: failure.error.message }
        : { code: null, message: failure.message },
    );
}

/**
 * Connects the store to the backend.
 *
 * Two directions at milestone 001:
 *
 * - **startup** — `config_get` supplies the preferences the backend persisted,
 *   which become the store's initial values;
 * - **push** — `error:notify` turns a backend error into a toast, including
 *   errors nobody asked for, such as a failed write.
 *
 * @returns the unsubscribe function for the event listener.
 */
export async function startBackendBridge(): Promise<() => void> {
  // Subscribing must not be able to sink the whole bridge. It rejects whenever
  // the Tauri API is unavailable — opening the dev server in a plain browser
  // is enough — and an unguarded `await` would then skip `config_get`
  // entirely, leaving the interface on its defaults with no explanation.
  //
  // The two directions are independent: failing to listen for pushed errors is
  // no reason to stop reading the preferences.
  let unlisten = () => undefined as void;

  try {
    unlisten = await onErrorNotify(({ code, message }) =>
      usePreferences.getState().notify({ code, message }),
    );
  } catch (cause) {
    usePreferences.getState().notify({
      code: null,
      message: cause instanceof Error ? cause.message : String(cause),
    });
  }

  const outcome = await configGet();

  if (outcome.ok) {
    adopt(outcome.value);
  } else {
    // Deliberately non-fatal. If only persistence is broken, the interface must
    // stay usable on its defaults — refusing to start would turn a recoverable
    // failure into a total one.
    report(outcome.failure);
  }

  return unlisten;
}

/** The preferences the settings screen can change at milestone 001. */
export type PreferencePatch = Partial<Pick<ConfigSetInput, "language" | "theme">>;

/**
 * Writes one preference and adopts the result.
 *
 * CA-001-02 requires preferences to survive a restart, so a change has to
 * reach `config_set` — updating the store alone would look right until the
 * next launch, then silently revert.
 *
 * `config_set` replaces the WHOLE configuration, so the patch is merged onto
 * the last configuration the backend confirmed. Submitting only the changed
 * field would reset `logLevel` and `retentionDays` to their defaults.
 *
 * The store is **not** updated optimistically. Showing a value the backend
 * refused is worse than a moment of latency: the interface would display a
 * preference that is not stored, and the discrepancy would only surface at the
 * next restart.
 */
export async function persistPreference(patch: PreferencePatch): Promise<void> {
  const current = usePreferences.getState().backendConfig;

  if (current === null) {
    // Nothing was ever read, so there is nothing to merge onto. Writing a
    // half-built configuration would be worse than not writing at all.
    usePreferences.getState().notify({
      code: null,
      message: "preferences have not been read from the backend yet",
    });

    return;
  }

  const outcome = await configSet({
    language: patch.language ?? current.language,
    theme: patch.theme ?? current.theme,
    logLevel: current.logLevel,
    retentionDays: current.retentionDays,
  });

  if (outcome.ok) {
    adopt(outcome.value);
    return;
  }

  // A `backend` failure has ALREADY been pushed on `error:notify` by the
  // command itself, so notifying here showed the same toast twice, each to be
  // dismissed separately. The event owns the notification; the return owns
  // marking the form — which is what a later milestone will do with it.
  //
  // A `transport` failure has no event, because Rust never ran: this is the
  // only path that must report.
  if (outcome.failure.kind === "transport") {
    report(outcome.failure);
  }
}
