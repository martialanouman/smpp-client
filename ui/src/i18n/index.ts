import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import en from "./locales/en.json";
import fr from "./locales/fr.json";

/**
 * i18n bootstrap — French by default, English as fallback (guide §10.2).
 *
 * Milestone 000 does not populate the catalogues: that is milestone 001's
 * job. But CLAUDE.md §4 forbids any hard-coded string in a component,
 * including on a placeholder page. Two keys beat a debt to undo next
 * milestone.
 */
// Catalogues are bundled, so `init` resolves immediately and does not need to
// be awaited. A top-level `await` would make this module a needless
// suspension point in the loading graph.
void i18n.use(initReactI18next).init({
  resources: {
    fr: { translation: fr },
    en: { translation: en },
  },
  lng: "fr",
  fallbackLng: "fr",
  interpolation: { escapeValue: false },
});

export default i18n;
