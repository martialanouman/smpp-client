import { useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  BindTypeDto,
  Gsm7CharsetDto,
  IdMatchingDto,
  Gsm7PackingDto,
  InterfaceVersionDto,
  SessionProfileDto,
} from "../../ipc";
import { useSessions } from "../../store/sessions";

const BIND_TYPES: readonly BindTypeDto[] = ["transmitter", "receiver", "transceiver"];
const VERSIONS: readonly InterfaceVersionDto[] = ["v3.4", "v5.0"];
const PACKINGS: readonly Gsm7PackingDto[] = ["unpacked", "packed"];
const CHARSETS: readonly Gsm7CharsetDto[] = ["gsm0338", "latin1"];

/**
 * The three identifier-matching policies, safest first.
 *
 * `bases` is last on purpose: it is the only lossy one — it reads the receipt's
 * identifier in the other base, which can land on a different message — and a
 * list ordered by tolerance puts the choice in the operator's hands in the
 * order they should consider it.
 */
const ID_MATCHINGS: readonly IdMatchingDto[] = ["exact", "relaxed", "bases"];

/** Tailwind classes shared by every control, so the form stays even. */
const CONTROL =
  "rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm";

interface FieldProps {
  readonly label: string;
  readonly children: React.ReactNode;
}

function Field({ label, children }: FieldProps) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-xs font-medium">{label}</span>
      {children}
    </label>
  );
}

interface ProfileFormProps {
  /** The profile to edit, or a blank one to create. */
  readonly initial: SessionProfileDto;
  /** Called once the profile has been saved, or the form dismissed. */
  readonly onDone: () => void;
}

/**
 * The connection profile form (EF-CNX-01).
 *
 * **No validation here.** Every bound is checked in Rust, which treats the
 * WebView as untrusted (CLAUDE.md §3), and a copy of the rules in TypeScript
 * would be a second source of truth that drifts. The form's job is to carry
 * the values and to show the refusal — which arrives as a translatable code.
 *
 * The numeric inputs use `type="number"` for the keyboard and the spinner, not
 * as a guarantee: `<input type="number">` happily yields an empty string, and
 * a hand-crafted `invoke` bypasses the control entirely.
 */
export function ProfileForm({ initial, onDone }: ProfileFormProps) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<SessionProfileDto>(initial);
  const save = useSessions((state) => state.save);
  const busy = useSessions((state) => state.busy);

  const set = <K extends keyof SessionProfileDto>(key: K, value: SessionProfileDto[K]) => {
    setDraft((current) => ({ ...current, [key]: value }));
  };

  const number = (key: keyof SessionProfileDto) => (value: string) => {
    // `Number("")` is 0, which would silently send a zero the backend then
    // refuses — a confusing error for an empty field. `NaN` is sent as is and
    // rejected as out of range, which at least names the field.
    set(key, Number(value) as never);
  };

  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();

        void save(draft).then((saved) => {
          if (saved) {
            onDone();
          }
        });
      }}
    >
      <div className="grid grid-cols-2 gap-3">
        <Field label={t("sessions.field.name")}>
          <input
            value={draft.name}
            onChange={(event) => set("name", event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.systemId")}>
          <input
            value={draft.systemId}
            onChange={(event) => set("systemId", event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.host")}>
          <input
            value={draft.host}
            onChange={(event) => set("host", event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.port")}>
          <input
            type="number"
            value={draft.port}
            onChange={(event) => number("port")(event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.bindType")}>
          <select
            value={draft.bindType}
            onChange={(event) => set("bindType", event.target.value as BindTypeDto)}
            className={CONTROL}
          >
            {BIND_TYPES.map((value) => (
              <option key={value} value={value}>
                {t(`sessions.bindType.${value}`)}
              </option>
            ))}
          </select>
        </Field>

        <Field label={t("sessions.field.interfaceVersion")}>
          <select
            value={draft.interfaceVersion}
            onChange={(event) => set("interfaceVersion", event.target.value as InterfaceVersionDto)}
            className={CONTROL}
          >
            {VERSIONS.map((value) => (
              <option key={value} value={value}>
                {value}
              </option>
            ))}
          </select>
        </Field>

        <Field label={t("sessions.field.systemType")}>
          <input
            value={draft.systemType}
            onChange={(event) => set("systemType", event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.enquireLinkS")}>
          <input
            type="number"
            value={draft.enquireLinkS}
            onChange={(event) => number("enquireLinkS")(event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.responseTimeoutS")}>
          <input
            type="number"
            value={draft.responseTimeoutS}
            onChange={(event) => number("responseTimeoutS")(event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.windowSize")}>
          <input
            type="number"
            value={draft.windowSize}
            onChange={(event) => number("windowSize")(event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.throughputTps")}>
          <input
            type="number"
            value={draft.throughputTps}
            onChange={(event) => number("throughputTps")(event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.minTps")}>
          <input
            type="number"
            value={draft.minTps}
            onChange={(event) => number("minTps")(event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.minBackoffS")}>
          <input
            type="number"
            value={draft.minBackoffS}
            onChange={(event) => number("minBackoffS")(event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.maxBackoffS")}>
          <input
            type="number"
            value={draft.maxBackoffS}
            onChange={(event) => number("maxBackoffS")(event.target.value)}
            className={CONTROL}
          />
        </Field>

        <Field label={t("sessions.field.gsm7Packing")}>
          <select
            value={draft.gsm7Packing}
            onChange={(event) => set("gsm7Packing", event.target.value as Gsm7PackingDto)}
            className={CONTROL}
          >
            {PACKINGS.map((value) => (
              <option key={value} value={value}>
                {t(`sessions.gsm7Packing.${value}`)}
              </option>
            ))}
          </select>
        </Field>

        <Field label={t("sessions.field.gsm7Charset")}>
          <select
            value={draft.gsm7Charset}
            onChange={(event) => set("gsm7Charset", event.target.value as Gsm7CharsetDto)}
            className={CONTROL}
          >
            {CHARSETS.map((value) => (
              <option key={value} value={value}>
                {t(`sessions.gsm7Charset.${value}`)}
              </option>
            ))}
          </select>
        </Field>

        <Field label={t("sessions.field.dlrIdMatching")}>
          <select
            value={draft.dlrIdMatching}
            onChange={(event) => set("dlrIdMatching", event.target.value as IdMatchingDto)}
            className={CONTROL}
          >
            {ID_MATCHINGS.map((value) => (
              <option key={value} value={value}>
                {t(`sessions.dlrIdMatching.${value}`)}
              </option>
            ))}
          </select>
        </Field>
      </div>

      {draft.dlrIdMatching === "bases" ? (
        <p role="note" className="text-xs text-[var(--shinobi-danger)]">
          {t("sessions.dlrIdMatchingWarning")}
        </p>
      ) : (
        <p className="text-xs opacity-70">{t("sessions.dlrIdMatchingHint")}</p>
      )}

      <p className="text-xs opacity-70">{t("sessions.throughputHint")}</p>

      <p className="text-xs opacity-70">{t("sessions.gsm7Hint")}</p>

      <div className="flex flex-wrap items-center gap-4">
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={draft.reconnectEnabled}
            onChange={(event) => set("reconnectEnabled", event.target.checked)}
          />
          {t("sessions.field.reconnectEnabled")}
        </label>

        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={draft.jitter}
            onChange={(event) => set("jitter", event.target.checked)}
          />
          {t("sessions.field.jitter")}
        </label>
      </div>

      <div className="flex gap-2">
        <button
          type="submit"
          disabled={busy}
          className="rounded-md bg-[var(--shinobi-accent)] px-3 py-2 text-sm font-medium disabled:opacity-50"
        >
          {t("sessions.save")}
        </button>

        <button
          type="button"
          onClick={onDone}
          className="rounded-md border border-[var(--shinobi-border)] px-3 py-2 text-sm hover:bg-[var(--shinobi-hover)]"
        >
          {t("sessions.cancel")}
        </button>
      </div>
    </form>
  );
}
