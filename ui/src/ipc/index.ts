/**
 * The backend boundary.
 *
 * `generated/bindings.ts` already carries the typed call functions; this module
 * adds the one thing a generator cannot: a **single failure shape**.
 *
 * A call can fail two ways, and conflating them is how a UI ends up showing
 * "unknown error":
 *
 * - `backend` — the command ran and returned an {@link ErrorDto}. It has a
 *   stable `code`, so the interface can translate it and point at the offending
 *   field.
 * - `transport` — the bridge itself failed: no backend, a serialisation
 *   mismatch, a missing capability. There is no `code` because Rust never
 *   produced one, and inventing one here would be hand-writing a piece of the
 *   contract that ADR 0003 requires to be generated.
 */

import type { UnlistenFn } from "@tauri-apps/api/event";

import { commands, events } from "./generated/bindings";
import type { AppConfig, ConfigSetInput, ErrorDto, ErrorNotify } from "./generated/bindings";

export type {
  AppConfig,
  ConfigSetInput,
  ErrorCode,
  ErrorDto,
  ErrorNotify,
  Language,
  LogLevel,
  RetentionDays,
  Theme,
} from "./generated/bindings";

/** Why a call produced no value. */
export type IpcFailure =
  | { readonly kind: "backend"; readonly error: ErrorDto }
  | { readonly kind: "transport"; readonly message: string };

/** The result of a backend call — never an exception. */
export type IpcOutcome<T> =
  { readonly ok: true; readonly value: T } | { readonly ok: false; readonly failure: IpcFailure };

/**
 * Runs a generated command and normalises both failure paths.
 *
 * The generated `typedError` helper re-throws anything that is a real `Error`,
 * which is exactly the transport case; a returned `{ status: "error" }` is the
 * backend case.
 */
async function call<T>(
  invocation: () => Promise<{ status: "ok"; data: T } | { status: "error"; error: ErrorDto }>,
): Promise<IpcOutcome<T>> {
  try {
    const result = await invocation();

    return result.status === "ok"
      ? { ok: true, value: result.data }
      : { ok: false, failure: { kind: "backend", error: result.error } };
  } catch (cause) {
    return {
      ok: false,
      failure: {
        kind: "transport",
        message: cause instanceof Error ? cause.message : String(cause),
      },
    };
  }
}

/** Reads the application preferences. */
export function configGet(): Promise<IpcOutcome<AppConfig>> {
  return call(() => commands.configGet());
}

/**
 * Writes the application preferences.
 *
 * The input is deliberately made of raw strings: validation belongs to the
 * backend, which treats the WebView as untrusted. Constraining it here would
 * only hide the error path that CA-001-05 requires to work.
 */
export function configSet(input: ConfigSetInput): Promise<IpcOutcome<AppConfig>> {
  return call(() => commands.configSet(input));
}

/**
 * Subscribes to `error:notify`.
 *
 * Returns the unsubscribe function; a component that forgets to call it on
 * unmount leaks a listener that keeps firing on a dead reducer.
 */
export function onErrorNotify(handler: (payload: ErrorNotify) => void): Promise<UnlistenFn> {
  return events.errorNotify.listen((event) => handler(event.payload));
}
