import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import en from "./locales/en.json";
import fr from "./locales/fr.json";

/**
 * Amorçage i18n — français par défaut, anglais en repli (guide §10.2).
 *
 * Le jalon 000 ne peuple pas les catalogues : c'est l'objet de step-001. Mais
 * CLAUDE.md §4 interdit toute chaîne en dur dans un composant, y compris sur
 * une page placeholder. Deux clés valent mieux qu'une dette à défaire au
 * jalon suivant.
 */
// Les catalogues sont embarqués : `init` résout immédiatement et n'a pas
// besoin d'être attendu. Un `await` de premier niveau ferait de ce module un
// point de suspension inutile dans le graphe de chargement.
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
