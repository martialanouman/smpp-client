import { useState } from "react";
import { useTranslation } from "react-i18next";

import { NO_FILTER, useLogs } from "../../store/logs";
import type { LogFilterInput } from "../../ipc";

/** The six states of spec §14.3, in lifecycle order. */
const STATES = ["QUEUED", "SENT", "ACCEPTED", "DELIVERED", "FAILED", "EXPIRED"] as const;

/**
 * The filter bar of the log screen (spec §13.3).
 *
 * # Applied on submit, not on every keystroke
 *
 * A full-text search over 200 000 rows is a scan; running one per keystroke
 * would send a dozen queries to type one word and show the results of the
 * wrong one. The form holds a draft and submits it, which also makes the
 * screen usable from the keyboard alone.
 *
 * # Nothing here validates
 *
 * A malformed date reaches the backend and comes back as `LOGS_INVALID_FILTER`
 * naming the field. That is deliberate: CLAUDE.md §3 treats the WebView as
 * untrusted, so the backend has to check anyway, and a second copy of the rule
 * here is a second thing to keep in step.
 */
export function LogFilters() {
  const { t } = useTranslation();
  const applied = useLogs((state) => state.filter);
  const setFilter = useLogs((state) => state.setFilter);
  const tab = useLogs((state) => state.tab);

  const [draft, setDraft] = useState<LogFilterInput>(applied);

  const update = (patch: Partial<LogFilterInput>) => {
    setDraft((current) => ({ ...current, ...patch }));
  };

  return (
    <form
      aria-label={t("logs.filters.label")}
      onSubmit={(event) => {
        event.preventDefault();
        setFilter(draft);
      }}
      className="flex flex-wrap items-end gap-3 rounded-md border border-[var(--shinobi-border)] p-3"
    >
      <Field label={t("logs.filters.search")}>
        <input
          type="search"
          value={draft.search ?? ""}
          onChange={(event) => {
            update({ search: event.target.value });
          }}
          className="w-48 rounded border border-[var(--shinobi-border)] bg-transparent px-2 py-1 text-sm"
        />
      </Field>

      <Field label={t("logs.filters.destPrefix")}>
        <input
          type="text"
          inputMode="tel"
          placeholder="+225"
          value={draft.destPrefix ?? ""}
          onChange={(event) => {
            update({ destPrefix: event.target.value });
          }}
          className="w-28 rounded border border-[var(--shinobi-border)] bg-transparent px-2 py-1 text-sm"
        />
      </Field>

      <Field label={t("logs.filters.state")}>
        <select
          value={draft.state ?? ""}
          disabled={tab !== "messages"}
          onChange={(event) => {
            update({ state: event.target.value === "" ? null : event.target.value });
          }}
          className="rounded border border-[var(--shinobi-border)] bg-transparent px-2 py-1 text-sm"
        >
          <option value="">{t("logs.filters.anyState")}</option>
          {STATES.map((state) => (
            <option key={state} value={state}>
              {t(`logs.state.${state}`)}
            </option>
          ))}
        </select>
      </Field>

      <Field label={t("logs.filters.dlrErr")}>
        <input
          type="text"
          value={draft.dlrErr ?? ""}
          onChange={(event) => {
            update({ dlrErr: event.target.value });
          }}
          className="w-24 rounded border border-[var(--shinobi-border)] bg-transparent px-2 py-1 text-sm"
        />
      </Field>

      <Field label={t("logs.filters.from")}>
        <input
          type="datetime-local"
          value={toLocal(draft.createdFrom)}
          onChange={(event) => {
            update({ createdFrom: fromLocal(event.target.value) });
          }}
          className="rounded border border-[var(--shinobi-border)] bg-transparent px-2 py-1 text-sm"
        />
      </Field>

      <Field label={t("logs.filters.to")}>
        <input
          type="datetime-local"
          value={toLocal(draft.createdTo)}
          onChange={(event) => {
            update({ createdTo: fromLocal(event.target.value) });
          }}
          className="rounded border border-[var(--shinobi-border)] bg-transparent px-2 py-1 text-sm"
        />
      </Field>

      <button
        type="submit"
        className="rounded-md bg-[var(--shinobi-accent)] px-3 py-1.5 text-sm font-medium"
      >
        {t("logs.filters.apply")}
      </button>

      <button
        type="button"
        onClick={() => {
          setDraft(NO_FILTER);
          setFilter(NO_FILTER);
        }}
        className="rounded-md border border-[var(--shinobi-border)] px-3 py-1.5 text-sm hover:bg-[var(--shinobi-hover)]"
      >
        {t("logs.filters.clear")}
      </button>
    </form>
  );
}

/** A labelled control. */
function Field({
  label,
  children,
}: {
  readonly label: string;
  readonly children: React.ReactNode;
}) {
  return (
    <label className="flex flex-col gap-1 text-xs text-[var(--shinobi-muted)]">
      {label}
      {children}
    </label>
  );
}

/**
 * Renders an RFC 3339 instant for a `datetime-local` input.
 *
 * The control speaks **local** time with no offset, and the contract speaks
 * UTC with a `Z`. The conversion is here, once, rather than in each of the two
 * date fields.
 */
function toLocal(instant: string | null): string {
  if (instant === null || instant === "") {
    return "";
  }

  const parsed = new Date(instant);

  if (Number.isNaN(parsed.getTime())) {
    return "";
  }

  const pad = (value: number) => String(value).padStart(2, "0");

  return (
    `${String(parsed.getFullYear())}-${pad(parsed.getMonth() + 1)}-${pad(parsed.getDate())}` +
    `T${pad(parsed.getHours())}:${pad(parsed.getMinutes())}`
  );
}

/**
 * Reads a `datetime-local` value back into RFC 3339 UTC.
 *
 * An empty box means "no bound", which is `null` and not "the epoch": the
 * backend reads an empty string as a cleared criterion, and sending one would
 * work by accident rather than by contract.
 */
function fromLocal(value: string): string | null {
  if (value === "") {
    return null;
  }

  const parsed = new Date(value);

  return Number.isNaN(parsed.getTime()) ? value : parsed.toISOString();
}
