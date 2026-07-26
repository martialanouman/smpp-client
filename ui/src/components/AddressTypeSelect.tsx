import { useTranslation } from "react-i18next";

/** The option value standing for "let the backend derive it". */
const DERIVED = "";

interface Props<T extends string> {
  /** Label shown above the field. */
  readonly label: string;
  /** The values to offer, in display order. */
  readonly values: readonly T[];
  /** i18n key prefix, so each value renders its documented meaning. */
  readonly namespace: string;
  /**
   * The value currently selected, or `null` for "derived".
   *
   * A field that accepts `null` offers a `derived` entry; one that does not
   * offers only the protocol values.
   */
  readonly value: T | null;
  /** Called with the new value, or `null` when "derived" is chosen. */
  readonly onChange: (value: T | null) => void;
  /**
   * Label of the "derived" entry.
   *
   * Its presence is what turns this into a nullable field. Omitting it means
   * the caller always has a value — a recipient's type of number, which has a
   * safe default rather than a derivation.
   */
  readonly derivedLabel?: string;
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
 * list they offer, which `components/addressTypes.ts` holds.
 *
 * # Why "derived" is an entry and not a disabled field
 *
 * The sender's type and plan are derived from the address unless the operator
 * chooses one. The field used to show `International` while sending `null`,
 * and to grey itself out when its neighbour was unset — so a choice the
 * operator made was silently dropped, and the value displayed was not the
 * value sent. CA-006-06 asks for the opposite: what the screen shows is what
 * travels.
 *
 * Making it an ordinary option says the same thing without a second rule: the
 * selected entry always describes what will be sent.
 */
export function AddressTypeSelect<T extends string>({
  label,
  values,
  namespace,
  value,
  onChange,
  derivedLabel,
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
        value={value ?? DERIVED}
        onChange={(event) => {
          // The sentinel goes back out as `null`, so the caller never has to
          // know an empty string stood for "derived".
          const chosen = event.target.value;

          onChange(chosen === DERIVED ? null : (chosen as T));
        }}
        className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
      >
        {derivedLabel === undefined ? null : (
          <option key={DERIVED} value={DERIVED}>
            {derivedLabel}
          </option>
        )}

        {values.map((option) => (
          <option key={option} value={option}>
            {t(`${namespace}.${option}`)}
          </option>
        ))}
      </select>
    </div>
  );
}
