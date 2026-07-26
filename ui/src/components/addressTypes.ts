import type { NpiDto, TonDto } from "../ipc";

/**
 * The order the TON and NPI drop-downs offer their values in.
 *
 * A file of its own rather than two constants beside the component that uses
 * them: a module that exports both a component and a value opts out of React's
 * fast refresh, which the lint rule points at.
 *
 * The **order** is the decision here, not the list — the list is the whole of
 * the spec §7.4 table and could have been derived from the generated union.
 * `international` and `isdn` come first because they are the answer nine times
 * out of ten, and a drop-down whose first entry is `unknown (0)` invites an
 * operator to leave it there.
 */

/** The seven type-of-number values of spec §7.4. */
export const TON_VALUES: readonly TonDto[] = [
  "international",
  "national",
  "networkSpecific",
  "subscriberNumber",
  "alphanumeric",
  "abbreviated",
  "unknown",
];

/** The seven numbering-plan values of spec §7.4. */
export const NPI_VALUES: readonly NpiDto[] = [
  "isdn",
  "national",
  "landMobile",
  "data",
  "telex",
  "private",
  "unknown",
];
