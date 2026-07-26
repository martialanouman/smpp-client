import { useTranslation } from "react-i18next";

import type { Screen } from "../store/preferences";

interface PlaceholderViewProps {
  /** The screen this placeholder stands in for. */
  readonly screen: Screen;
}

/**
 * Stands in for a screen whose implementation lands in a later milestone.
 *
 * It states what the screen will hold rather than showing an empty page: the
 * navigation is meant to be walkable at milestone 001, and a blank panel reads
 * as a bug.
 */
export function PlaceholderView({ screen }: PlaceholderViewProps) {
  const { t } = useTranslation();

  return (
    <div className="flex max-w-lg flex-col gap-2">
      <p className="text-sm opacity-70">{t(`screen.${screen}`)}</p>
      <div className="rounded-md border border-dashed border-[var(--shinobi-border)] p-6">
        <p className="text-sm font-medium">{t("empty.title")}</p>
        <p className="mt-1 text-sm opacity-70">{t("empty.body")}</p>
      </div>
    </div>
  );
}
