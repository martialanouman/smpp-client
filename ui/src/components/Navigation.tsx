import { useTranslation } from "react-i18next";

import { SCREENS, usePreferences } from "../store/preferences";

/**
 * The eight-screen sidebar.
 *
 * Buttons rather than links: there is no router and no URL, so a `<a href>`
 * would be a lie to assistive technology and would let a middle-click open a
 * window the WebView cannot serve. `aria-current` carries the selected state,
 * which the colour alone would not convey.
 */
export function Navigation() {
  const { t } = useTranslation();
  const current = usePreferences((state) => state.screen);
  const goTo = usePreferences((state) => state.goTo);

  return (
    <nav aria-label={t("nav.label")} className="flex w-56 shrink-0 flex-col gap-1 p-3">
      <p className="px-3 pb-3 text-lg font-semibold tracking-tight">{t("app.name")}</p>

      {SCREENS.map((screen) => {
        const active = screen === current;

        return (
          <button
            key={screen}
            type="button"
            aria-current={active ? "page" : "false"}
            onClick={() => goTo(screen)}
            className={[
              "rounded-md px-3 py-2 text-left text-sm transition-colors",
              active ? "bg-[var(--shinobi-accent)] font-medium" : "hover:bg-[var(--shinobi-hover)]",
            ].join(" ")}
          >
            {t(`nav.${screen}`)}
          </button>
        );
      })}
    </nav>
  );
}
