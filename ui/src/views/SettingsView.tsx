import { useTranslation } from "react-i18next";

import type { Language, Theme } from "../ipc";
import { persistPreference } from "../store/bridge";
import { usePreferences } from "../store/preferences";

const LANGUAGES: readonly Language[] = ["fr", "en"];
const THEMES: readonly Theme[] = ["light", "dark", "system"];

/**
 * Language and theme.
 *
 * The only screen with real controls at milestone 001 — the other seven are
 * placeholders. Log level and retention belong here too but wait for the
 * backend that consumes them (milestones 002 and 014).
 *
 * Native `<select>` elements on purpose: they are keyboard accessible, they
 * carry their label, and the OS renders them. A custom dropdown would have to
 * re-earn all three.
 */
export function SettingsView() {
  const { t } = useTranslation();
  const language = usePreferences((state) => state.language);
  const theme = usePreferences((state) => state.theme);
  // Writes go through the backend rather than the store: CA-001-02 requires
  // preferences to survive a restart, and the store is only the in-memory
  // mirror. `persistPreference` adopts whatever the backend confirms, so a
  // refused value never reaches the screen.

  return (
    <div className="flex max-w-md flex-col gap-6">
      <label className="flex flex-col gap-1">
        <span className="text-sm font-medium">{t("settings.language")}</span>
        <select
          value={language}
          onChange={(event) => void persistPreference({ language: event.target.value as Language })}
          className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
        >
          {LANGUAGES.map((value) => (
            <option key={value} value={value}>
              {value === "fr" ? "Français" : "English"}
            </option>
          ))}
        </select>
      </label>

      <label className="flex flex-col gap-1">
        <span className="text-sm font-medium">{t("settings.theme")}</span>
        <select
          value={theme}
          onChange={(event) => void persistPreference({ theme: event.target.value as Theme })}
          className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
        >
          {THEMES.map((value) => (
            <option key={value} value={value}>
              {t(`settings.theme${value.charAt(0).toUpperCase()}${value.slice(1)}`)}
            </option>
          ))}
        </select>
      </label>
    </div>
  );
}
