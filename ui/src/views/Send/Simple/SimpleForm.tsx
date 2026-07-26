import { useTranslation } from "react-i18next";

import { AddressTypeSelect } from "../../../components/AddressTypeSelect";
import { NPI_VALUES, TON_VALUES } from "../../../components/addressTypes";
import { CharacterCounter } from "../../../components/CharacterCounter";
import { TlvEditor } from "../../../components/TlvEditor";
import type {
  EncodingDto,
  MessagePreviewDto,
  RegisteredDeliveryDto,
  SegmentationModeDto,
  SessionProfileDto,
  SessionStatusDto,
} from "../../../ipc";
import type { SendForm } from "../../../store/send";

/** The encodings the DCS selector offers, automatic first. */
const ENCODINGS: readonly EncodingDto[] = ["automatic", "gsm7Bit", "latin1", "ucs2"];

/** The three concatenation modes, the portable one first. */
const MODES: readonly SegmentationModeDto[] = ["udh", "sar", "messagePayload"];

/** What `registered_delivery` may ask for. */
const RECEIPTS: readonly RegisteredDeliveryDto[] = ["onAnyOutcome", "onFailure", "none"];

/** A session a message can actually be sent on. */
const BOUND = "BOUND";

interface Props {
  readonly profiles: readonly SessionProfileDto[];
  readonly statuses: Readonly<Record<string, SessionStatusDto>>;
  readonly sessionId: string;
  readonly form: SendForm;
  readonly preview: MessagePreviewDto | null;
  readonly sending: boolean;
  readonly onSession: (sessionId: string) => void;
  readonly onChange: (form: SendForm) => void;
  readonly onSubmit: () => void;
}

/**
 * A text field with its label, since the screen has fourteen of them.
 *
 * The hint is attached through `aria-describedby` rather than left inside the
 * `<label>`. A wrapping label makes its **whole** text content the accessible
 * name, so "service_type" plus its sentence of guidance would be read out as
 * one forty-word field name — and `getByLabelText("send.serviceType")` would
 * not match it either, which is how the mistake surfaced.
 */
function Field({
  id,
  label,
  hint,
  value,
  onChange,
  placeholder,
  mono = false,
}: {
  readonly id: string;
  readonly label: string;
  readonly hint?: string;
  readonly value: string;
  readonly onChange: (value: string) => void;
  readonly placeholder?: string;
  readonly mono?: boolean;
}) {
  return (
    <div className="flex flex-col gap-1">
      <label htmlFor={id} className="text-xs font-medium">
        {label}
      </label>
      <input
        id={id}
        value={value}
        placeholder={placeholder}
        aria-describedby={hint === undefined ? undefined : `${id}-hint`}
        onChange={(event) => onChange(event.target.value)}
        className={`rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm ${
          mono ? "font-mono" : ""
        }`}
      />
      {hint === undefined ? null : (
        <span id={`${id}-hint`} className="text-xs opacity-60">
          {hint}
        </span>
      )}
    </div>
  );
}

/**
 * Send › Simple (deliverable L-006-06).
 *
 * Every field of spec §7.3 the operator controls, the live counter, the TLV
 * editor. The layout separates what is used on every send from what is used
 * once a year: the recipient, the sender and the body are above the fold, the
 * twelve protocol fields are behind a `<details>`.
 *
 * **No protocol logic.** The counter, the encoding and the segment count all
 * arrive from `message_preview`; nothing here parses a number, counts a
 * septet or decides what is valid (CLAUDE.md §3).
 */
export function SimpleForm({
  profiles,
  statuses,
  sessionId,
  form,
  preview,
  sending,
  onSession,
  onChange,
  onSubmit,
}: Props) {
  const { t } = useTranslation();

  const bound = profiles.filter((profile) => statuses[profile.sessionId ?? ""]?.state === BOUND);

  // A sender made of anything but digits forces `source_addr_ton = 5`, and
  // some message centres refuse it outright. Spec §7.4 and fiche §6: say so
  // here rather than let the operator discover a rejection.
  const source = form.source ?? "";
  const alphanumericSender = source.trim() !== "" && !/^\+?\d+$/u.test(source.trim());

  const ready = bound.length > 0 && sessionId !== "" && form.destination.trim() !== "";

  return (
    <form
      className="flex max-w-3xl flex-col gap-5"
      onSubmit={(event) => {
        event.preventDefault();
        onSubmit();
      }}
    >
      <div className="flex flex-col gap-1">
        <label htmlFor="send-session" className="text-xs font-medium">
          {t("send.session")}
        </label>
        <select
          id="send-session"
          value={sessionId}
          onChange={(event) => onSession(event.target.value)}
          className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
        >
          <option value="">{t("send.sessionPlaceholder")}</option>
          {bound.map((profile) => (
            <option key={profile.sessionId ?? ""} value={profile.sessionId ?? ""}>
              {profile.name} — {profile.host}:{profile.port}
            </option>
          ))}
        </select>
        {bound.length === 0 ? (
          <span className="text-xs text-amber-700 dark:text-amber-300">
            {t("send.noBoundSession")}
          </span>
        ) : null}
      </div>

      <div className="grid gap-3 sm:grid-cols-3">
        <Field
          id="send-destination"
          label={t("send.destination")}
          hint={t("send.destinationHint")}
          placeholder="+225 01 02 03 04 05"
          value={form.destination}
          onChange={(destination) => onChange({ ...form, destination })}
        />
        <AddressTypeSelect
          label={t("send.destTon")}
          values={TON_VALUES}
          namespace="send.ton"
          value={form.destTon}
          onChange={(destTon) => onChange({ ...form, destTon })}
        />
        <AddressTypeSelect
          label={t("send.destNpi")}
          values={NPI_VALUES}
          namespace="send.npi"
          value={form.destNpi}
          onChange={(destNpi) => onChange({ ...form, destNpi })}
        />
      </div>

      <div className="grid gap-3 sm:grid-cols-3">
        <Field
          id="send-source"
          label={t("send.source")}
          hint={t("send.sourceHint")}
          value={source}
          onChange={(value) => onChange({ ...form, source: value === "" ? null : value })}
        />
        <AddressTypeSelect
          label={t("send.sourceTon")}
          values={TON_VALUES}
          namespace="send.ton"
          value={form.sourceTon ?? "international"}
          onChange={(sourceTon) => onChange({ ...form, sourceTon })}
          disabled={form.sourceNpi === null}
        />
        <AddressTypeSelect
          label={t("send.sourceNpi")}
          values={NPI_VALUES}
          namespace="send.npi"
          value={form.sourceNpi ?? "isdn"}
          onChange={(sourceNpi) => onChange({ ...form, sourceNpi })}
        />
      </div>

      {alphanumericSender ? (
        <p className="rounded-md bg-amber-500/10 px-3 py-2 text-xs text-amber-800 dark:text-amber-200">
          {t("send.alphanumericWarning")}
        </p>
      ) : null}

      <div className="flex flex-col gap-1">
        <label htmlFor="send-text" className="text-xs font-medium">
          {t("send.text")}
        </label>
        <textarea
          id="send-text"
          rows={5}
          value={form.text}
          onChange={(event) => onChange({ ...form, text: event.target.value })}
          className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
        />
        <CharacterCounter preview={preview} />
      </div>

      <div className="grid gap-3 sm:grid-cols-3">
        <label className="flex flex-col gap-1">
          <span className="text-xs font-medium">{t("send.encodingLabel")}</span>
          <select
            value={form.encoding}
            onChange={(event) => onChange({ ...form, encoding: event.target.value as EncodingDto })}
            className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
          >
            {ENCODINGS.map((option) => (
              <option key={option} value={option}>
                {t(`send.encoding.${option}`)}
              </option>
            ))}
          </select>
        </label>

        <label className="flex flex-col gap-1">
          <span className="text-xs font-medium">{t("send.mode")}</span>
          <select
            value={form.segmentationMode}
            onChange={(event) =>
              onChange({
                ...form,
                segmentationMode: event.target.value as SegmentationModeDto,
              })
            }
            className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
          >
            {MODES.map((option) => (
              <option key={option} value={option}>
                {t(`send.modes.${option}`)}
              </option>
            ))}
          </select>
        </label>

        <label className="flex flex-col gap-1">
          <span className="text-xs font-medium">{t("send.registeredDelivery")}</span>
          <select
            value={form.registeredDelivery}
            onChange={(event) =>
              onChange({
                ...form,
                registeredDelivery: event.target.value as RegisteredDeliveryDto,
              })
            }
            className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
          >
            {RECEIPTS.map((option) => (
              <option key={option} value={option}>
                {t(`send.receipts.${option}`)}
              </option>
            ))}
          </select>
        </label>
      </div>

      <details className="rounded-md border border-[var(--shinobi-border)] p-4">
        <summary className="cursor-pointer text-sm font-medium">{t("send.advanced")}</summary>

        <div className="mt-4 flex flex-col gap-4">
          <div className="grid gap-3 sm:grid-cols-3">
            <Field
              id="send-serviceType"
              label={t("send.serviceType")}
              hint={t("send.serviceTypeHint")}
              value={form.serviceType}
              onChange={(serviceType) => onChange({ ...form, serviceType })}
            />
            <Field
              id="send-protocolId"
              label={t("send.protocolId")}
              value={String(form.protocolId)}
              mono
              onChange={(value) => onChange({ ...form, protocolId: clampOctet(value) })}
            />
            <Field
              id="send-priorityFlag"
              label={t("send.priorityFlag")}
              hint={t("send.priorityFlagHint")}
              value={String(form.priorityFlag)}
              mono
              onChange={(value) => onChange({ ...form, priorityFlag: clampOctet(value) })}
            />
          </div>

          <div className="grid gap-3 sm:grid-cols-3">
            <Field
              id="send-scheduleDeliveryTime"
              label={t("send.scheduleDeliveryTime")}
              hint={t("send.timeHint")}
              placeholder="YYMMDDhhmmsstnnp"
              mono
              value={form.scheduleDeliveryTime}
              onChange={(scheduleDeliveryTime) => onChange({ ...form, scheduleDeliveryTime })}
            />
            <Field
              id="send-validityPeriod"
              label={t("send.validityPeriod")}
              hint={t("send.timeHint")}
              placeholder="YYMMDDhhmmsstnnp"
              mono
              value={form.validityPeriod}
              onChange={(validityPeriod) => onChange({ ...form, validityPeriod })}
            />
            <Field
              id="send-smDefaultMsgId"
              label={t("send.smDefaultMsgId")}
              value={String(form.smDefaultMsgId)}
              mono
              onChange={(value) => onChange({ ...form, smDefaultMsgId: clampOctet(value) })}
            />
          </div>

          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={form.replaceIfPresent}
              onChange={(event) => onChange({ ...form, replaceIfPresent: event.target.checked })}
            />
            {t("send.replaceIfPresent")}
          </label>

          <TlvEditor tlvs={form.tlvs} onChange={(tlvs) => onChange({ ...form, tlvs })} />
        </div>
      </details>

      <div>
        <button
          type="submit"
          disabled={sending || !ready}
          className="rounded-md bg-[var(--shinobi-accent)] px-4 py-2 text-sm font-medium disabled:opacity-50"
        >
          {sending ? t("send.sending") : t("send.submit")}
        </button>
      </div>
    </form>
  );
}

/**
 * Keeps a typed octet inside `0..=255`.
 *
 * The one piece of arithmetic on this screen, and it is a widget concern
 * rather than a protocol one: the field is a `u8` on the wire, and a `number`
 * input that let `300` through would be rejected at the bridge with a
 * deserialisation error carrying no `ErrorCode`.
 */
function clampOctet(raw: string): number {
  const parsed = Number.parseInt(raw, 10);

  if (Number.isNaN(parsed)) {
    return 0;
  }

  return Math.min(255, Math.max(0, parsed));
}
