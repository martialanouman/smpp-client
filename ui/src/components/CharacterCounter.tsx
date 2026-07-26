import { useTranslation } from "react-i18next";

import type { MessagePreviewDto } from "../ipc";

interface Props {
  /** What the backend computed, or `null` while it has not answered yet. */
  readonly preview: MessagePreviewDto | null;
}

/**
 * The live counter of the message editor (CA-006-09).
 *
 * Four numbers and one word, and **not one of them is computed here**: the
 * encoding, the characters, the units used, the units left in the segment and
 * the segment count all come from `message_preview`. A `text.length` in this
 * component would be wrong on the first `€` — two septets, not one — and would
 * then disagree with the segments actually sent.
 *
 * Characters and units are shown side by side rather than conflated, because
 * they part ways exactly where an operator gets surprised: ten characters
 * containing three `€` occupy thirteen septets.
 */
export function CharacterCounter({ preview }: Props) {
  const { t } = useTranslation();

  if (preview === null) {
    return (
      <p className="text-xs opacity-60" aria-live="polite">
        {t("send.counter.pending")}
      </p>
    );
  }

  return (
    <p className="flex flex-wrap gap-x-4 gap-y-1 text-xs opacity-80" aria-live="polite">
      <span>
        {t("send.counter.encoding")}{" "}
        <strong className="font-medium">{t(`send.encoding.${preview.encoding}`)}</strong>
        <span className="opacity-60"> (DCS {preview.dataCoding})</span>
      </span>

      <span>{t("send.counter.characters", { count: preview.characters })}</span>

      <span>
        {t("send.counter.units", {
          used: preview.unitsUsed,
          remaining: preview.unitsRemaining,
        })}
      </span>

      <span>{t("send.counter.segments", { count: preview.segments })}</span>
    </p>
  );
}
