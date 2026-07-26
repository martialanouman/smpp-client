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
import type {
  AppConfig,
  ConfigSetInput,
  ErrorDto,
  ErrorNotify,
  SessionBindInput,
  SessionProfileDto,
  SessionStatusDto,
  SessionsState,
} from "./generated/bindings";

export type {
  AppConfig,
  BindTypeDto,
  ConfigSetInput,
  ErrorCode,
  ErrorDto,
  ErrorNotify,
  Gsm7CharsetDto,
  Gsm7PackingDto,
  InterfaceVersionDto,
  Language,
  LogLevel,
  RetentionDays,
  SessionBindInput,
  SessionProfileDto,
  SessionStatusDto,
  SessionsState,
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
 * Narrows an unknown rejection to an {@link ErrorDto}.
 *
 * `typedError` only re-throws values that are real `Error` instances, so a
 * rejection that is *not* an `Error` reaches us as `{ status: "error" }` —
 * which does not make it a DTO. Tauri rejects argument deserialisation with a
 * bare JSON **string**: send `retentionDays: -1` through a hand-made `invoke`
 * and the rejection is
 * `"invalid args \`input\` for command \`config_set\`: …"`.
 *
 * Without this check that string was labelled `backend`, and reading `.code`
 * and `.message` off it produced `undefined` — an empty toast, with neither
 * code nor message. Exactly the class of input CA-001-05 is about.
 */
function isErrorDto(value: unknown): value is ErrorDto {
  return (
    typeof value === "object" &&
    value !== null &&
    "code" in value &&
    typeof (value as { code: unknown }).code === "string" &&
    "message" in value &&
    typeof (value as { message: unknown }).message === "string"
  );
}

/**
 * Runs a generated command and normalises both failure paths.
 *
 * Anything that is not a well-formed DTO is classed `transport`, and that is
 * the honest label: Rust never produced a `code` for it.
 */
async function call<T>(
  invocation: () => Promise<{ status: "ok"; data: T } | { status: "error"; error: ErrorDto }>,
): Promise<IpcOutcome<T>> {
  try {
    const result = await invocation();

    if (result.status === "ok") {
      return { ok: true, value: result.data };
    }

    return isErrorDto(result.error)
      ? { ok: false, failure: { kind: "backend", error: result.error } }
      : {
          ok: false,
          failure: {
            kind: "transport",
            message: typeof result.error === "string" ? result.error : JSON.stringify(result.error),
          },
        };
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

/** Lists every connection profile, oldest first. */
export function sessionList(): Promise<IpcOutcome<SessionProfileDto[]>> {
  return call(() => commands.sessionList());
}

/** Creates a connection profile. */
export function sessionCreate(input: SessionProfileDto): Promise<IpcOutcome<SessionProfileDto>> {
  return call(() => commands.sessionCreate(input));
}

/** Updates a connection profile. */
export function sessionUpdate(input: SessionProfileDto): Promise<IpcOutcome<SessionProfileDto>> {
  return call(() => commands.sessionUpdate(input));
}

/** Deletes a connection profile, closing its session first. */
export function sessionDelete(sessionId: string): Promise<IpcOutcome<boolean>> {
  return call(() => commands.sessionDelete(sessionId));
}

/**
 * Opens a session.
 *
 * The password travels on this call and on no other. It is not held in the
 * store, not written to the profile, and never comes back: the backend turns
 * it into an opaque value the moment it arrives.
 */
export function sessionBind(input: SessionBindInput): Promise<IpcOutcome<SessionStatusDto>> {
  return call(() => commands.sessionBind(input));
}

/** Closes a session cleanly. */
export function sessionUnbind(sessionId: string): Promise<IpcOutcome<boolean>> {
  return call(() => commands.sessionUnbind(sessionId));
}

/** Reads the live state of one session. */
export function sessionStatus(sessionId: string): Promise<IpcOutcome<SessionStatusDto>> {
  return call(() => commands.sessionStatus(sessionId));
}

/**
 * Subscribes to `sessions:state`.
 *
 * Returns the unsubscribe function; a component that forgets to call it on
 * unmount leaks a listener that keeps firing on a dead reducer.
 */
export function onSessionsState(handler: (payload: SessionsState) => void): Promise<UnlistenFn> {
  return events.sessionsState.listen((event) => handler(event.payload));
}
