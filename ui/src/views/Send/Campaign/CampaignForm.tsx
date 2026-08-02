import { useState } from "react";
import { useTranslation } from "react-i18next";

import { AddressTypeSelect } from "../../../components/AddressTypeSelect";
import { NPI_VALUES, TON_VALUES } from "../../../components/addressTypes";
import type {
  CampaignCreateInput,
  CampaignSendConfigInput,
  ContactListDto,
  EncodingDto,
  RegisteredDeliveryDto,
  RetryBackoffInput,
  SegmentationModeDto,
  SessionProfileDto,
  SessionStatusDto,
} from "../../../ipc";

/** The encodings the DCS selector offers, automatic first. */
const ENCODINGS: readonly EncodingDto[] = ["automatic", "gsm7Bit", "latin1", "ucs2"];

/** The three concatenation modes, the portable one first. */
const MODES: readonly SegmentationModeDto[] = ["udh", "sar", "messagePayload"];

/** What `registered_delivery` may ask for. */
const RECEIPTS: readonly RegisteredDeliveryDto[] = ["onAnyOutcome", "onFailure", "none"];

/** How the wait between two attempts grows. */
const BACKOFFS: readonly RetryBackoffInput[] = ["exponential", "fixed"];

/** A session a campaign can actually be started on. */
const BOUND = "BOUND";

/**
 * What the form holds, before it is turned into a {@link CampaignCreateInput}.
 *
 * A flat shape on purpose: the nested one the backend takes is assembled once,
 * on submit, so no field of this form is two levels deep in an `onChange`.
 */
interface FormState {
  name: string;
  template: string;
  sessionId: string;
  listId: string;
  source: string;
  destTon: CampaignSendConfigInput["destTon"];
  destNpi: CampaignSendConfigInput["destNpi"];
  encoding: EncodingDto;
  segmentationMode: SegmentationModeDto;
  registeredDelivery: RegisteredDeliveryDto;
  substitute: boolean;
  substituteValue: string;
  abandonUnanswered: boolean;
  maxAttempts: number;
  initialDelayS: number;
  maxDelayS: number;
  backoff: RetryBackoffInput;
  startAt: string;
  windowed: boolean;
  windowOpen: string;
  windowClose: string;
  offsetMinutes: number;
}

/**
 * The defaults a campaign starts from.
 *
 * Every one of them is the conservative half of the choice the backend also
 * defaults to: reject a recipient whose variable is missing rather than send
 * half a greeting, ask for a delivery receipt, replay three times. The form and
 * `CampaignPlan::new` agree because both were written from spec §10.2 — and if
 * they ever drift, the backend's is the one that runs.
 */
const EMPTY: FormState = {
  name: "",
  template: "",
  sessionId: "",
  listId: "",
  source: "",
  destTon: "international",
  destNpi: "isdn",
  encoding: "automatic",
  segmentationMode: "udh",
  registeredDelivery: "onAnyOutcome",
  substitute: false,
  substituteValue: "",
  abandonUnanswered: false,
  maxAttempts: 3,
  initialDelayS: 5,
  maxDelayS: 300,
  backoff: "exponential",
  startAt: "",
  windowed: false,
  windowOpen: "08:00",
  windowClose: "20:00",
  offsetMinutes: 0,
};

interface Props {
  readonly profiles: readonly SessionProfileDto[];
  readonly statuses: Readonly<Record<string, SessionStatusDto>>;
  readonly lists: readonly ContactListDto[];
  readonly creating: boolean;
  readonly onCreate: (input: CampaignCreateInput) => void;
}

/**
 * Send › Campagne — the creation form (deliverable L-010-08).
 *
 * **No validation of its own beyond "is the submit button enabled".** Every
 * rule — the template's syntax, the retry bounds, the `HH:MM` window, the
 * sender's length — belongs to `messaging` and is applied by
 * `campaign_create`, which treats this form as untrusted (CLAUDE.md §3). A
 * second set of rules here is a second set to keep in step, and the one that
 * decides is the backend's.
 *
 * The layout puts what every campaign needs above the fold — name, session,
 * list, template — and the replay policy and the planning behind a `<details>`,
 * for the same reason the unit form does: they are set once a year.
 */
export function CampaignForm({ profiles, statuses, lists, creating, onCreate }: Props) {
  const { t } = useTranslation();
  const [form, setForm] = useState<FormState>(EMPTY);

  const bound = profiles.filter((profile) => statuses[profile.sessionId ?? ""]?.state === BOUND);
  const ready =
    !creating && form.name.trim() !== "" && form.template.trim() !== "" && form.sessionId !== "";

  const set = <K extends keyof FormState>(key: K, value: FormState[K]) =>
    setForm((previous) => ({ ...previous, [key]: value }));

  const submit = () => {
    const config: CampaignSendConfigInput = {
      sessionId: form.sessionId,
      listId: form.listId === "" ? null : form.listId,
      excludedListIds: [],
      source: form.source.trim() === "" ? null : form.source.trim(),
      destTon: form.destTon,
      destNpi: form.destNpi,
      encoding: form.encoding,
      segmentationMode: form.segmentationMode,
      registeredDelivery: form.registeredDelivery,
      onMissingVariable: form.substitute
        ? { policy: "substitute", value: form.substituteValue }
        : { policy: "reject" },
      onUnanswered: form.abandonUnanswered ? "abandon" : "reemit",
      retry: {
        maxAttempts: form.maxAttempts,
        initialDelayS: form.initialDelayS,
        maxDelayS: form.maxDelayS,
        backoff: form.backoff,
      },
      schedule: {
        startAt: instant(form.startAt),
        window: form.windowed
          ? {
              open: form.windowOpen,
              close: form.windowClose,
              offsetMinutes: form.offsetMinutes,
            }
          : null,
      },
    };

    onCreate({ name: form.name.trim(), template: form.template, config });
  };

  return (
    <form
      className="flex max-w-3xl flex-col gap-5"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="flex flex-col gap-1">
          <label htmlFor="campaign-name" className="text-xs font-medium">
            {t("campaign.name")}
          </label>
          <input
            id="campaign-name"
            value={form.name}
            onChange={(event) => set("name", event.target.value)}
            className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
          />
        </div>

        <div className="flex flex-col gap-1">
          <label htmlFor="campaign-session" className="text-xs font-medium">
            {t("campaign.session")}
          </label>
          <select
            id="campaign-session"
            value={form.sessionId}
            onChange={(event) => set("sessionId", event.target.value)}
            className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
          >
            <option value="">{t("campaign.sessionPlaceholder")}</option>
            {bound.map((profile) => (
              <option key={profile.sessionId ?? ""} value={profile.sessionId ?? ""}>
                {profile.name} — {profile.host}:{profile.port}
              </option>
            ))}
          </select>
          {bound.length === 0 ? (
            <span className="text-xs text-amber-700 dark:text-amber-300">
              {t("campaign.noBoundSession")}
            </span>
          ) : null}
        </div>
      </div>

      <div className="flex flex-col gap-1">
        <label htmlFor="campaign-list" className="text-xs font-medium">
          {t("campaign.list")}
        </label>
        <select
          id="campaign-list"
          value={form.listId}
          onChange={(event) => set("listId", event.target.value)}
          className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
        >
          <option value="">{t("campaign.allContacts")}</option>
          {lists.map((list) => (
            <option key={list.listId} value={list.listId}>
              {list.name}
            </option>
          ))}
        </select>
        <span className="text-xs opacity-60">{t("campaign.listHint")}</span>
      </div>

      <div className="flex flex-col gap-1">
        <label htmlFor="campaign-template" className="text-xs font-medium">
          {t("campaign.template")}
        </label>
        <textarea
          id="campaign-template"
          rows={4}
          value={form.template}
          aria-describedby="campaign-template-hint"
          onChange={(event) => set("template", event.target.value)}
          className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
        />
        <span id="campaign-template-hint" className="text-xs opacity-60">
          {t("campaign.templateHint")}
        </span>
      </div>

      <div className="grid gap-3 sm:grid-cols-3">
        <div className="flex flex-col gap-1">
          <label htmlFor="campaign-source" className="text-xs font-medium">
            {t("campaign.source")}
          </label>
          <input
            id="campaign-source"
            value={form.source}
            aria-describedby="campaign-source-hint"
            onChange={(event) => set("source", event.target.value)}
            className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
          />
          <span id="campaign-source-hint" className="text-xs opacity-60">
            {t("campaign.sourceHint")}
          </span>
        </div>
        <AddressTypeSelect
          label={t("campaign.destTon")}
          values={TON_VALUES}
          namespace="send.ton"
          value={form.destTon}
          onChange={(value) => set("destTon", value ?? "international")}
        />
        <AddressTypeSelect
          label={t("campaign.destNpi")}
          values={NPI_VALUES}
          namespace="send.npi"
          value={form.destNpi}
          onChange={(value) => set("destNpi", value ?? "isdn")}
        />
      </div>

      <fieldset className="flex flex-col gap-2 rounded-md border border-[var(--shinobi-border)] p-3">
        <legend className="px-1 text-xs font-medium">{t("campaign.missing.legend")}</legend>

        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={form.substitute}
            onChange={(event) => set("substitute", event.target.checked)}
          />
          {t("campaign.missing.substitute")}
        </label>

        {form.substitute ? (
          <div className="flex flex-col gap-1">
            <label htmlFor="campaign-substitute" className="text-xs font-medium">
              {t("campaign.missing.value")}
            </label>
            <input
              id="campaign-substitute"
              value={form.substituteValue}
              onChange={(event) => set("substituteValue", event.target.value)}
              className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
            />
          </div>
        ) : (
          <span className="text-xs opacity-60">{t("campaign.missing.rejectHint")}</span>
        )}
      </fieldset>

      <details className="rounded-md border border-[var(--shinobi-border)] p-3">
        <summary className="cursor-pointer text-sm font-medium">{t("campaign.advanced")}</summary>

        <div className="mt-3 flex flex-col gap-4">
          <div className="grid gap-3 sm:grid-cols-3">
            <Choice
              id="campaign-encoding"
              label={t("campaign.encoding")}
              values={ENCODINGS}
              namespace="send.encoding"
              value={form.encoding}
              onChange={(value) => set("encoding", value)}
            />
            <Choice
              id="campaign-mode"
              label={t("campaign.mode")}
              values={MODES}
              namespace="send.modes"
              value={form.segmentationMode}
              onChange={(value) => set("segmentationMode", value)}
            />
            <Choice
              id="campaign-receipt"
              label={t("campaign.registeredDelivery")}
              values={RECEIPTS}
              namespace="send.receipts"
              value={form.registeredDelivery}
              onChange={(value) => set("registeredDelivery", value)}
            />
          </div>

          <fieldset className="flex flex-col gap-2 rounded-md border border-[var(--shinobi-border)] p-3">
            <legend className="px-1 text-xs font-medium">{t("campaign.retry.legend")}</legend>

            <div className="grid gap-3 sm:grid-cols-4">
              <Number
                id="campaign-attempts"
                label={t("campaign.retry.maxAttempts")}
                value={form.maxAttempts}
                onChange={(value) => set("maxAttempts", value)}
              />
              <Number
                id="campaign-initial-delay"
                label={t("campaign.retry.initialDelay")}
                value={form.initialDelayS}
                onChange={(value) => set("initialDelayS", value)}
              />
              <Number
                id="campaign-max-delay"
                label={t("campaign.retry.maxDelay")}
                value={form.maxDelayS}
                onChange={(value) => set("maxDelayS", value)}
              />
              <Choice
                id="campaign-backoff"
                label={t("campaign.retry.backoff")}
                values={BACKOFFS}
                namespace="campaign.backoff"
                value={form.backoff}
                onChange={(value) => set("backoff", value)}
              />
            </div>

            <span className="text-xs opacity-60">{t("campaign.retry.hint")}</span>
          </fieldset>

          <fieldset className="flex flex-col gap-2 rounded-md border border-[var(--shinobi-border)] p-3">
            <legend className="px-1 text-xs font-medium">{t("campaign.schedule.legend")}</legend>

            <div className="flex flex-col gap-1">
              <label htmlFor="campaign-start-at" className="text-xs font-medium">
                {t("campaign.schedule.startAt")}
              </label>
              <input
                id="campaign-start-at"
                type="datetime-local"
                value={form.startAt}
                aria-describedby="campaign-start-at-hint"
                onChange={(event) => set("startAt", event.target.value)}
                className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
              />
              <span id="campaign-start-at-hint" className="text-xs opacity-60">
                {t("campaign.schedule.startAtHint")}
              </span>
            </div>

            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={form.windowed}
                onChange={(event) => set("windowed", event.target.checked)}
              />
              {t("campaign.schedule.windowed")}
            </label>

            {form.windowed ? (
              <div className="grid gap-3 sm:grid-cols-3">
                <div className="flex flex-col gap-1">
                  <label htmlFor="campaign-window-open" className="text-xs font-medium">
                    {t("campaign.schedule.open")}
                  </label>
                  <input
                    id="campaign-window-open"
                    type="time"
                    value={form.windowOpen}
                    onChange={(event) => set("windowOpen", event.target.value)}
                    className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
                  />
                </div>
                <div className="flex flex-col gap-1">
                  <label htmlFor="campaign-window-close" className="text-xs font-medium">
                    {t("campaign.schedule.close")}
                  </label>
                  <input
                    id="campaign-window-close"
                    type="time"
                    value={form.windowClose}
                    onChange={(event) => set("windowClose", event.target.value)}
                    className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
                  />
                </div>
                <Number
                  id="campaign-offset"
                  label={t("campaign.schedule.offset")}
                  value={form.offsetMinutes}
                  onChange={(value) => set("offsetMinutes", value)}
                />
              </div>
            ) : null}

            <span className="text-xs opacity-60">{t("campaign.schedule.hint")}</span>
          </fieldset>

          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={form.abandonUnanswered}
              onChange={(event) => set("abandonUnanswered", event.target.checked)}
            />
            {t("campaign.unanswered.abandon")}
          </label>
          <span className="text-xs opacity-60">{t("campaign.unanswered.hint")}</span>
        </div>
      </details>

      <div>
        <button
          type="submit"
          disabled={!ready}
          className="rounded-md bg-[var(--shinobi-accent)] px-4 py-2 text-sm font-medium text-white disabled:opacity-40"
        >
          {creating ? t("campaign.creating") : t("campaign.create")}
        </button>
      </div>
    </form>
  );
}

/**
 * Reads a `datetime-local` value as an RFC 3339 instant, or `null`.
 *
 * **The conversion is the point.** `<input type="datetime-local">` yields
 * `2026-08-02T20:00` with no zone, meaning eight in the evening *where the
 * operator is*, and the backend parses an instant. Appending a `Z` — the
 * obvious shortcut — would read it as eight in the evening UTC, which in
 * Abidjan is right and in Los Angeles is seven hours early: a campaign that
 * starts in the middle of the afternoon. `Date` parses the zoneless form as
 * local time, which is what the operator typed.
 *
 * An unparseable value comes back as `null` rather than throwing. The backend
 * then simply starts the campaign at once, which is the same thing an empty
 * field asks for.
 */
function instant(local: string): string | null {
  if (local === "") {
    return null;
  }

  const parsed = new Date(local);

  return globalThis.Number.isNaN(parsed.getTime()) ? null : parsed.toISOString();
}

/** A labelled selector over a closed set of translated values. */
function Choice<T extends string>({
  id,
  label,
  values,
  namespace,
  value,
  onChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly values: readonly T[];
  readonly namespace: string;
  readonly value: T;
  readonly onChange: (value: T) => void;
}) {
  const { t } = useTranslation();

  return (
    <div className="flex flex-col gap-1">
      <label htmlFor={id} className="text-xs font-medium">
        {label}
      </label>
      <select
        id={id}
        value={value}
        onChange={(event) => onChange(event.target.value as T)}
        className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
      >
        {values.map((entry) => (
          <option key={entry} value={entry}>
            {t(`${namespace}.${entry}`)}
          </option>
        ))}
      </select>
    </div>
  );
}

/**
 * A labelled whole-number field.
 *
 * An empty box reads as `0` rather than as `NaN`: the backend refuses a zero
 * where zero is out of bounds, with a message naming the field, which is a
 * better answer than a form that silently posts nothing.
 */
function Number({
  id,
  label,
  value,
  onChange,
}: {
  readonly id: string;
  readonly label: string;
  readonly value: number;
  readonly onChange: (value: number) => void;
}) {
  return (
    <div className="flex flex-col gap-1">
      <label htmlFor={id} className="text-xs font-medium">
        {label}
      </label>
      <input
        id={id}
        type="number"
        value={value}
        onChange={(event) => onChange(globalThis.Number(event.target.value) || 0)}
        className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
      />
    </div>
  );
}
