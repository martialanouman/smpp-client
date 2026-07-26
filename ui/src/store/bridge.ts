import { configGet, onErrorNotify } from "../ipc";
import { usePreferences } from "./preferences";

/**
 * Connects the store to the backend.
 *
 * Two directions, and only two at milestone 001:
 *
 * - **startup** — `config_get` supplies the preferences the backend persisted,
 *   which become the store's initial values;
 * - **push** — `error:notify` turns a backend error into a toast, including
 *   errors nobody asked for, such as a failed write.
 *
 * The reverse direction (store to backend, on a settings change) waits for the
 * validation loop of milestone 002: writing on every keystroke would produce a
 * file write per character.
 *
 * @returns the unsubscribe function for the event listener.
 */
export async function startBackendBridge(): Promise<() => void> {
  const { notify, setLanguage, setTheme } = usePreferences.getState();

  const unlisten = await onErrorNotify(({ code, message }) => notify({ code, message }));

  const outcome = await configGet();

  if (outcome.ok) {
    setLanguage(outcome.value.language);
    setTheme(outcome.value.theme);
  } else {
    // Deliberately non-fatal. If only persistence is broken, the interface must
    // stay usable on its defaults — refusing to start would turn a recoverable
    // failure into a total one.
    notify(
      outcome.failure.kind === "backend"
        ? { code: outcome.failure.error.code, message: outcome.failure.error.message }
        : { code: null, message: outcome.failure.message },
    );
  }

  return unlisten;
}
