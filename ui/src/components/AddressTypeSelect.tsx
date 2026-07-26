import { useTranslation } from "react-i18next";

/**
 * A documented drop-down over a closed protocol enum.
 *
 * "Documented" is the requirement of the fiche and it is not decoration: `3`
 * means nothing, `network specific (3)` means something, and the octet is
 * shown beside the label because that is what an operator finds in their
 * message centre's documentation.
 *
 * One component for TON and NPI rather than two: the two differ only by the
 * list they offer, which `components/addressTypes.ts` holds.
 */

interface Props<T extends string> {
  /** Label shown above the field. */
  readonly label: string;
  /** The values to offer, in display order. */
  readonly values: readonly T[];
  /** i18n key prefix, so each value renders its documented meaning. */
  readonly namespace: string;
  /** The value currently selected. */
  readonly value: T;
  /** Called with the new value. */
  readonly onChange: (value: T) => void;
  /** Whether the field is greyed out — a derived TON, for instance. */
  readonly disabled?: boolean;
}

/**
 * A documented drop-down over a closed protocol enum.
 *
 * "Documented" is the requirement of the fiche and it is not decoration: `3`
 * means nothing, `network specific (3)` means something, and the octet is
 * shown beside the label because that is what an operator finds in their
 * message centre's documentation.
 *
 * One component for TON and NPI rather than two: the two differ only by the
 * list they offer.
 */
export function AddressTypeSelect<T extends string>({
  label,
  values,
  namespace,
  value,
  onChange,
  disabled = false,
}: Props<T>) {
  const { t } = useTranslation();

  // A generated identifier rather than one passed in: the accessible name has
  // to be the label alone, and the twelve call sites should not each have to
  // invent a unique string for that.
  const id = `${namespace}-${label}`.replace(/\s+/gu, "-");

  return (
    <div className="flex flex-col gap-1">
      <label htmlFor={id} className="text-xs font-medium">
        {label}
      </label>
      <select
        id={id}
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value as T)}
        className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm disabled:opacity-50"
      >
        {values.map((option) => (
          <option key={option} value={option}>
            {t(`${namespace}.${option}`)}
          </option>
        ))}
      </select>
    </div>
  );
}
