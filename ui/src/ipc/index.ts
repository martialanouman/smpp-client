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
  CampaignCreateInput,
  CampaignPageDto,
  CampaignProgressEvent,
  ConfigSetInput,
  ContactListDto,
  ContactPageDto,
  ImportOptionsInput,
  ImportProfileDto,
  ImportProgressEvent,
  ImportReportDto,
  ImportSourceInput,
  SelectionInput,
  LogFilterInput,
  LogPageDto,
  OrphanPageDto,
  PduPageDto,
  ErrorDto,
  ErrorNotify,
  MessagePreviewDto,
  MessagePreviewInput,
  MessageSendInput,
  MessageSendResultDto,
  MessageUpdate,
  MetricsTick,
  SessionBindInput,
  SessionProfileDto,
  SessionStatusDto,
  SessionsState,
} from "./generated/bindings";

export type {
  AppConfig,
  AttributeColumnInput,
  BindTypeDto,
  CampaignCreateInput,
  CampaignPageDto,
  CampaignProgressEvent,
  CampaignRowDto,
  CampaignSendConfigInput,
  ColumnMappingInput,
  ColumnRefInput,
  CombinationInput,
  ContactListDto,
  ContactPageDto,
  ContactRowDto,
  ConfigSetInput,
  EncodingDto,
  ErrorCode,
  ErrorDto,
  ErrorNotify,
  Gsm7CharsetDto,
  DailyWindowInput,
  DeduplicationInput,
  Gsm7PackingDto,
  HeaderModeInput,
  IdMatchingDto,
  MissingVariableInput,
  ImportOptionsInput,
  ImportProfileDto,
  ImportProgressEvent,
  ImportReportDto,
  ImportSourceInput,
  InterfaceVersionDto,
  Language,
  LogFilterInput,
  LogLevel,
  LogPageDto,
  LogRowDto,
  MessagePreviewDto,
  MessagePreviewInput,
  MessageSendInput,
  MessageSendResultDto,
  MessageUpdate,
  MetricsTick,
  MessageUpdateEntry,
  NpiDto,
  OrphanPageDto,
  OrphanRowDto,
  PduPageDto,
  PduRowDto,
  ReasonCountDto,
  RegisteredDeliveryDto,
  RejectedRowDto,
  RetentionDays,
  RetryBackoffInput,
  RetryInput,
  ScheduleInput,
  SelectionInput,
  SegmentOutcomeDto,
  SegmentationModeDto,
  SessionBindInput,
  SessionProfileDto,
  SessionStatusDto,
  SessionsState,
  Theme,
  TlvDto,
  TonDto,
  UnansweredInput,
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

/**
 * Sends one message (EF-MSG-01).
 *
 * A message the message centre **rejected** comes back as a successful
 * outcome whose `state` is `FAILED`, carrying the raw `command_status` and its
 * label: ENF-UTI-02 requires the operator to read the centre's own answer, and
 * turning it into an `IpcFailure` would replace it with one of ours.
 */
export function messageSend(input: MessageSendInput): Promise<IpcOutcome<MessageSendResultDto>> {
  return call(() => commands.messageSend(input));
}

/**
 * Recomputes the editor's counter.
 *
 * Called on every keystroke, and deliberately a backend call: the encoding
 * table, the escape characters and the segment budget are protocol rules, and
 * CLAUDE.md §3 keeps every one of them out of the WebView. A copy here would
 * be a second implementation that could disagree with the segments actually
 * sent — which is precisely what CA-006-09 forbids.
 */
export function messagePreview(input: MessagePreviewInput): Promise<IpcOutcome<MessagePreviewDto>> {
  return call(() => commands.messagePreview(input));
}

/**
 * Subscribes to `message:update`.
 *
 * Each payload carries a **batch** of transitions — the commit the receipt
 * pipeline just made — rather than a single message. A unit send produces
 * batches of one; a message centre replaying a backlog produces batches of two
 * hundred, which is the whole point (CA-008-08).
 *
 * Returns the unsubscribe function; a component that forgets to call it on
 * unmount leaks a listener that keeps firing on a dead reducer.
 */
export function onMessageUpdate(handler: (payload: MessageUpdate) => void): Promise<UnlistenFn> {
  return events.messageUpdate.listen((event) => handler(event.payload));
}

/**
 * Subscribes to `metrics:tick` (spec §9.6, §18.1).
 *
 * The backend emits at a fixed 4 Hz per live session, whatever the throughput
 * — see `METRICS_TICK_INTERVAL` in `events.rs`. Each payload is a **reading**,
 * not a delta, so a missed one costs a frame of animation and nothing else.
 *
 * Returns the unsubscribe function; a component that forgets to call it on
 * unmount leaks a listener that keeps firing on a dead reducer.
 */
export function onMetricsTick(handler: (payload: MetricsTick) => void): Promise<UnlistenFn> {
  return events.metricsTick.listen((event) => handler(event.payload));
}

/**
 * Reads one page of the business journal (EF-LOG-01).
 *
 * The bulk of the log crosses **here**, page by page, and never as events:
 * CA-008-08 keeps `message:update` to aggregated increments so that two
 * hundred thousand rows cannot be pushed through the bridge.
 *
 * `cursor` is opaque — hand back whatever the previous page returned in
 * `next`, and `null` to start over. It is a string because it is a 64-bit row
 * identifier, which JSON cannot carry as a number without precision loss.
 */
export function logsQuery(
  filter: LogFilterInput,
  cursor: string | null,
  limit: number | null,
): Promise<IpcOutcome<LogPageDto>> {
  return call(() => commands.logsQuery(filter, cursor, limit));
}

/**
 * Reads one page of the delivery receipts that correlated to no message
 * (CA-008-04).
 *
 * A separate call rather than a filter on {@link logsQuery}: an orphan has no
 * message behind it, so it has no state, no recipient and no send instant. One
 * table with half its columns permanently empty would be a worse screen than
 * two tables.
 */
export function logsOrphans(
  sessionId: string | null,
  cursor: string | null,
  limit: number | null,
): Promise<IpcOutcome<OrphanPageDto>> {
  return call(() => commands.logsOrphans(sessionId, cursor, limit));
}

/**
 * Reads one page of the PDU log (CA-008-09).
 *
 * The payload says whether recording is **on**, so an empty table can explain
 * itself: "nothing recorded" and "recording is off" are different states, and
 * a screen showing the same emptiness for both sends an operator hunting a bug
 * that is a switch.
 */
export function logsPdus(
  sessionId: string | null,
  cursor: string | null,
  limit: number | null,
): Promise<IpcOutcome<PduPageDto>> {
  return call(() => commands.logsPdus(sessionId, cursor, limit));
}

/**
 * Turns PDU recording on or off (CA-008-09).
 *
 * Returns the state **in force**, not the one requested: the interface shows
 * what happened rather than what it asked for.
 */
export function logsSetPduLogging(enabled: boolean): Promise<IpcOutcome<boolean>> {
  return call(() => commands.logsSetPduLogging(enabled));
}

/**
 * Reads a contact file and writes what it holds (EF-CNT-01).
 *
 * Resolves when the import is **over** — a million-row file means a promise
 * pending for minutes, which is why {@link onImportProgress} exists. The
 * report comes back even when the operator cancelled, with `cancelled` set and
 * `imported` counting exactly what reached the database (CA-009-10).
 */
export function contactsImport(
  source: ImportSourceInput,
  options: ImportOptionsInput,
): Promise<IpcOutcome<ImportReportDto>> {
  return call(() => commands.contactsImport(source, options));
}

/**
 * Asks the running import to stop (CA-009-10).
 *
 * Resolves to `false` when there was nothing to cancel, which is not an error:
 * an operator clicking cancel on an import that has just finished has got what
 * they wanted.
 */
export function contactsCancelImport(): Promise<IpcOutcome<boolean>> {
  return call(() => commands.contactsCancelImport());
}

/**
 * Reads one page of the contacts table.
 *
 * `cursor` is opaque — hand back whatever the previous page returned in
 * `next`, and `null` to start over. `total` counts the **selection** and not
 * the search, so the virtualised scrollbar keeps its size while the operator
 * types.
 */
export function contactsPage(
  selection: SelectionInput | null,
  search: string | null,
  cursor: string | null,
  limit: number | null,
): Promise<IpcOutcome<ContactPageDto>> {
  return call(() => commands.contactsPage(selection, search, cursor, limit));
}

/**
 * Creates a contact list (CA-009-12).
 *
 * Resolves to its identifier, which the import assistant then sends as
 * `listId`.
 */
export function contactsCreateList(name: string): Promise<IpcOutcome<string>> {
  return call(() => commands.contactsCreateList(name));
}

/** Every contact list, oldest first (CA-009-12). Unpaginated by design. */
export function contactsLists(): Promise<IpcOutcome<ContactListDto[]>> {
  return call(() => commands.contactsLists());
}

/** Every saved column-mapping profile, oldest first (CA-009-09). */
export function contactsProfiles(): Promise<IpcOutcome<ImportProfileDto[]>> {
  return call(() => commands.contactsProfiles());
}

/**
 * Saves a column-mapping profile (CA-009-09).
 *
 * Resolves to the identifier in force — the one sent, or a fresh one when the
 * form sent none. That is how a form that has just created a profile learns
 * what to send next time instead of creating a second one.
 */
export function contactsSaveProfile(profile: ImportProfileDto): Promise<IpcOutcome<string>> {
  return call(() => commands.contactsSaveProfile(profile));
}

/**
 * Subscribes to import progress (CA-009-11).
 *
 * The importer already paces these — about one per thousand rows — so there is
 * no throttle on this side. The **last** event carries `done`, and it is the
 * one a progress bar needs; a throttle here would be free to drop it.
 *
 * Returns the unsubscribe function. Call it on unmount.
 */
export function onImportProgress(
  handler: (payload: ImportProgressEvent) => void,
): Promise<UnlistenFn> {
  return events.importProgress.listen((event) => handler(event.payload));
}

/**
 * Creates a campaign (EF-MSG-02, spec §10.2).
 *
 * Validates the whole configuration, counts the recipients the selection picks
 * out and stores the campaign as `VALIDATED`. Resolves to its identifier.
 *
 * Nothing is sent: starting is {@link campaignStart}, and it is a separate
 * click on purpose — a form that both configured and launched a send to two
 * hundred thousand people would have no step at which to change one's mind.
 */
export function campaignCreate(input: CampaignCreateInput): Promise<IpcOutcome<string>> {
  return call(() => commands.campaignCreate(input));
}

/**
 * One page of campaigns, oldest first.
 *
 * `cursor` is opaque — hand back whatever the previous page returned in `next`,
 * and `null` to start over.
 */
export function campaignList(
  cursor: string | null,
  limit: number | null,
): Promise<IpcOutcome<CampaignPageDto>> {
  return call(() => commands.campaignList(cursor, limit));
}

/**
 * Starts a campaign (spec §10.3).
 *
 * Resolves as soon as the campaign is **running**, not when it has finished: a
 * campaign of half a million recipients runs for hours. Follow it with
 * {@link onCampaignProgress}, and read the per-message detail on the Journaux
 * screen.
 */
export function campaignStart(campaignId: string): Promise<IpcOutcome<null>> {
  return call(() => commands.campaignStart(campaignId));
}

/**
 * Suspends the feeding of a running campaign (CA-010-03).
 *
 * The messages already in the send window finish normally and the session stays
 * bound; only the feeding stops.
 */
export function campaignPause(campaignId: string): Promise<IpcOutcome<null>> {
  return call(() => commands.campaignPause(campaignId));
}

/**
 * Resumes a campaign (CA-010-03).
 *
 * One command for two situations the interface does not have to tell apart: a
 * campaign paused a moment ago carries on where it was, and a campaign a crash
 * or a restart left behind is run again in resuming mode — which asks the
 * journal about every recipient before emitting, so nothing already accepted
 * goes out twice (CA-010-05).
 */
export function campaignResume(campaignId: string): Promise<IpcOutcome<null>> {
  return call(() => commands.campaignResume(campaignId));
}

/** Stops a campaign for good (CA-010-09). */
export function campaignCancel(campaignId: string): Promise<IpcOutcome<null>> {
  return call(() => commands.campaignCancel(campaignId));
}

/**
 * Subscribes to `campaign:progress` (CA-010-11).
 *
 * The backend samples the campaign's counters at a fixed 4 Hz whatever the
 * throughput — see `CAMPAIGN_PROGRESS_INTERVAL` in `events.rs` — so a campaign
 * at ten thousand messages a second produces the same four events a second as
 * one at ten. Each payload is a **reading**, not a delta, so a missed one costs
 * a frame and nothing else.
 *
 * The **last** event of a run carries `done` and the terminal status. It is
 * emitted outside the paced loop and is never throttled, which is what stops a
 * finished campaign leaving a progress bar running for ever.
 *
 * The payload carries no throughput: the rate comes from the `metrics:tick` of
 * the session it names — see {@link onMetricsTick}. One measurement, one place.
 *
 * Returns the unsubscribe function. Call it on unmount.
 */
export function onCampaignProgress(
  handler: (payload: CampaignProgressEvent) => void,
): Promise<UnlistenFn> {
  return events.campaignProgress.listen((event) => handler(event.payload));
}

/**
 * Opens the native file picker and returns the chosen path, or `null`.
 *
 * The picker runs in the **backend**, not here: it is what lets the backend
 * remember which files the operator pointed at, so `contacts_import` can
 * refuse every other path. The WebView holds no filesystem or dialog
 * permission at all as a result.
 *
 * `null` means the operator dismissed the picker — an outcome, not a failure.
 */
export function pickContactFile(): Promise<IpcOutcome<string | null>> {
  return call(() => commands.contactsPickFile());
}
