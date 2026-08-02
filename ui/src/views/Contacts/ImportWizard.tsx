import { useState } from "react";
import { useTranslation } from "react-i18next";

import { pickContactFile } from "../../ipc";
import type {
  ColumnRefInput,
  DeduplicationInput,
  HeaderModeInput,
  ImportSourceInput,
} from "../../ipc";
import { useContacts } from "../../store/contacts";

/**
 * The import assistant (CA-009-03, CA-009-04, CA-009-06, CA-009-09).
 *
 * # Why the file is picked and not typed
 *
 * The WebView has no filesystem permission at all — `dialog:default` returns a
 * path the operator chose in a native picker, and the backend is what opens
 * it. A text box would let a page name any file on the disk, which is the
 * capability CLAUDE.md §8 declines to grant.
 *
 * # Why the mapping is a column name and not a preview grid
 *
 * A preview means reading the head of the file, which means a second command
 * and a second path through the reader. The backend already detects the
 * mapping from the header row when the operator leaves the field empty
 * (`ColumnMapping::detect`), so the field is an override rather than a
 * requirement — and typing a header name is what an operator does anyway once
 * the detection got it wrong.
 */
export function ImportWizard() {
  const { t } = useTranslation();

  const importing = useContacts((state) => state.importing);
  const progress = useContacts((state) => state.progress);
  const lists = useContacts((state) => state.lists);
  const runImport = useContacts((state) => state.runImport);
  const cancelImport = useContacts((state) => state.cancelImport);

  const [path, setPath] = useState<string | null>(null);
  const [sheet, setSheet] = useState("");
  const [msisdnColumn, setMsisdnColumn] = useState("");
  const [countryColumn, setCountryColumn] = useState("");
  const [headers, setHeaders] = useState<HeaderModeInput>("detect");
  const [defaultRegion, setDefaultRegion] = useState("");
  const [mobilesOnly, setMobilesOnly] = useState(false);
  const [deduplication, setDeduplication] = useState<DeduplicationInput>("firstWins");
  const [listId, setListId] = useState("");

  const isWorkbook = path !== null && /\.xlsx?$/i.test(path);

  const pick = async () => {
    const outcome = await pickContactFile();

    if (outcome.ok && outcome.value !== null) {
      setPath(outcome.value);
    }
  };

  const start = async () => {
    if (path === null) return;

    const source: ImportSourceInput = isWorkbook
      ? { kind: "xlsx", path, sheet: sheet === "" ? null : sheet }
      : { kind: "csv", path };

    await runImport(source, {
      mapping: {
        msisdn: columnRef(msisdnColumn),
        country: countryColumn === "" ? null : columnRef(countryColumn),
        attributes: [],
      },
      headers,
      defaultRegion: defaultRegion === "" ? null : defaultRegion.toUpperCase(),
      mobilesOnly,
      deduplication,
      listId: listId === "" ? null : listId,
    });
  };

  return (
    <section
      aria-labelledby="contacts-import-heading"
      className="flex flex-col gap-4 rounded-md border border-[var(--shinobi-border)] p-4"
    >
      <h2 id="contacts-import-heading" className="text-lg font-medium">
        {t("contacts.import.heading")}
      </h2>

      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={() => void pick()}
          disabled={importing}
          className="rounded-md border border-[var(--shinobi-border)] px-3 py-2 text-sm hover:bg-[var(--shinobi-hover)] disabled:opacity-50"
        >
          {t("contacts.import.pick")}
        </button>
        <p className="truncate text-sm text-[var(--shinobi-muted)]" title={path ?? ""}>
          {path ?? t("contacts.import.noFile")}
        </p>
      </div>

      <div className="grid gap-4 sm:grid-cols-2">
        {isWorkbook ? (
          <Field label={t("contacts.import.sheet")} hint={t("contacts.import.sheetHint")}>
            <input
              type="text"
              value={sheet}
              onChange={(event) => setSheet(event.target.value)}
              className="rounded-md border border-[var(--shinobi-border)] bg-transparent px-3 py-2"
            />
          </Field>
        ) : null}

        <Field label={t("contacts.import.msisdnColumn")} hint={t("contacts.import.columnHint")}>
          <input
            type="text"
            value={msisdnColumn}
            onChange={(event) => setMsisdnColumn(event.target.value)}
            placeholder={t("contacts.import.columnPlaceholder")}
            className="rounded-md border border-[var(--shinobi-border)] bg-transparent px-3 py-2"
          />
        </Field>

        <Field label={t("contacts.import.countryColumn")} hint={t("contacts.import.countryHint")}>
          <input
            type="text"
            value={countryColumn}
            onChange={(event) => setCountryColumn(event.target.value)}
            className="rounded-md border border-[var(--shinobi-border)] bg-transparent px-3 py-2"
          />
        </Field>

        <Field label={t("contacts.import.headers")}>
          <select
            value={headers}
            onChange={(event) => setHeaders(event.target.value as HeaderModeInput)}
            className="rounded-md border border-[var(--shinobi-border)] bg-transparent px-3 py-2"
          >
            <option value="detect">{t("contacts.import.headerMode.detect")}</option>
            <option value="present">{t("contacts.import.headerMode.present")}</option>
            <option value="absent">{t("contacts.import.headerMode.absent")}</option>
          </select>
        </Field>

        <Field label={t("contacts.import.defaultRegion")} hint={t("contacts.import.regionHint")}>
          <input
            type="text"
            value={defaultRegion}
            onChange={(event) => setDefaultRegion(event.target.value)}
            placeholder="CI"
            maxLength={2}
            className="rounded-md border border-[var(--shinobi-border)] bg-transparent px-3 py-2 uppercase"
          />
        </Field>

        <Field label={t("contacts.import.deduplication")}>
          <select
            value={deduplication}
            onChange={(event) => setDeduplication(event.target.value as DeduplicationInput)}
            className="rounded-md border border-[var(--shinobi-border)] bg-transparent px-3 py-2"
          >
            <option value="firstWins">{t("contacts.import.dedup.firstWins")}</option>
            <option value="mergeAttributes">{t("contacts.import.dedup.mergeAttributes")}</option>
          </select>
          <span className="text-xs text-[var(--shinobi-muted)]">
            {deduplication === "mergeAttributes" ? t("contacts.import.dedup.mergeWarning") : ""}
          </span>
        </Field>

        <Field label={t("contacts.import.list")} hint={t("contacts.import.listHint")}>
          <select
            value={listId}
            onChange={(event) => setListId(event.target.value)}
            className="rounded-md border border-[var(--shinobi-border)] bg-transparent px-3 py-2"
          >
            <option value="">{t("contacts.import.noList")}</option>
            {lists.map((list) => (
              <option key={list.listId} value={list.listId}>
                {list.name}
              </option>
            ))}
          </select>
        </Field>
      </div>

      <label className="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={mobilesOnly}
          onChange={(event) => setMobilesOnly(event.target.checked)}
        />
        {t("contacts.import.mobilesOnly")}
      </label>

      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={() => void start()}
          disabled={importing || path === null || msisdnColumn === ""}
          className="rounded-md bg-[var(--shinobi-accent)] px-4 py-2 text-sm font-medium disabled:opacity-50"
        >
          {t("contacts.import.start")}
        </button>

        {importing ? (
          <button
            type="button"
            onClick={() => void cancelImport()}
            className="rounded-md border border-[var(--shinobi-border)] px-4 py-2 text-sm"
          >
            {t("contacts.import.cancel")}
          </button>
        ) : null}
      </div>

      {importing ? (
        <p aria-live="polite" className="text-sm text-[var(--shinobi-muted)]">
          {progress === null
            ? t("contacts.import.starting")
            : t("contacts.import.progress", {
                processed: progress.processed,
                imported: progress.imported,
                rejected: progress.rejected,
                duplicates: progress.duplicates,
              })}
        </p>
      ) : null}
    </section>
  );
}

/**
 * Reads a column designation.
 *
 * A run of digits is a zero-based position, anything else is a header name.
 * The ambiguity is real — a file could have a column literally named `3` — and
 * it is resolved in favour of the position because a header row of bare
 * integers is a file with no header row.
 */
function columnRef(raw: string): ColumnRefInput {
  const trimmed = raw.trim();

  return /^\d+$/.test(trimmed)
    ? { by: "index", value: Number.parseInt(trimmed, 10) }
    : { by: "name", value: trimmed };
}

interface FieldProps {
  readonly label: string;
  readonly hint?: string;
  readonly children: React.ReactNode;
}

/** One labelled form control with an optional hint under it. */
function Field({ label, hint, children }: FieldProps) {
  return (
    <label className="flex flex-col gap-1 text-sm">
      <span>{label}</span>
      {children}
      {hint === undefined ? null : (
        <span className="text-xs text-[var(--shinobi-muted)]">{hint}</span>
      )}
    </label>
  );
}
