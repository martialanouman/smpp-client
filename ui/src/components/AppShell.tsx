import { useEffect } from "react";
import { useTranslation } from "react-i18next";

import { usePreferences } from "../store/preferences";
import { DashboardView } from "../views/Dashboard/DashboardView";
import { PlaceholderView } from "../views/PlaceholderView";
import { SendView } from "../views/Send/SendView";
import { SessionsView } from "../views/Sessions/SessionsView";
import { SettingsView } from "../views/SettingsView";
import { Navigation } from "./Navigation";
import { Notifications } from "./Notifications";

/**
 * Application shell: navigation, current screen, notifications.
 *
 * Holds the two effects that mirror preferences onto things React does not
 * own — the i18next instance and the document root. Doing this here rather
 * than inside the settings screen matters: the preferences also arrive from
 * the backend at startup, and a screen the user has not opened cannot apply
 * them.
 */
export function AppShell() {
  const { t } = useTranslation();
  const screen = usePreferences((state) => state.screen);
  const language = usePreferences((state) => state.language);
  const theme = usePreferences((state) => state.theme);
  const { i18n } = useTranslation();

  useEffect(() => {
    if (i18n.language !== language) {
      void i18n.changeLanguage(language);
    }
  }, [i18n, language]);

  useEffect(() => {
    // `system` means "no opinion": removing the attribute hands the decision
    // back to the `prefers-color-scheme` media query in styles.css, which is
    // what following the OS actually requires.
    if (theme === "system") {
      document.documentElement.removeAttribute("data-theme");
    } else {
      document.documentElement.setAttribute("data-theme", theme);
    }
  }, [theme]);

  return (
    <div className="flex min-h-screen">
      <Navigation />

      <main className="flex-1 border-l border-[var(--shinobi-border)] p-8">
        <h1 className="mb-6 text-2xl font-semibold tracking-tight">{t(`nav.${screen}`)}</h1>

        {screen === "settings" ? <SettingsView /> : null}
        {screen === "sessions" ? <SessionsView /> : null}
        {screen === "send" ? <SendView /> : null}
        {screen === "dashboard" ? <DashboardView /> : null}
        {screen !== "settings" &&
        screen !== "sessions" &&
        screen !== "send" &&
        screen !== "dashboard" ? (
          <PlaceholderView screen={screen} />
        ) : null}
      </main>

      <Notifications />
    </div>
  );
}
