import { useTranslation } from "react-i18next";

/**
 * Page d'attente du jalon 000.
 *
 * Elle n'existe que pour prouver que la chaîne complète tient debout —
 * Vite compile, React monte, i18n résout, la WebView Tauri affiche. Le shell
 * applicatif réel (navigation, vues métier) arrive au jalon 001.
 */
export function PlaceholderView() {
  const { t } = useTranslation();

  return (
    <main className="flex min-h-screen flex-col items-center justify-center gap-3 p-8 text-center">
      <h1 className="text-3xl font-semibold tracking-tight">{t("app.name")}</h1>
      <p className="max-w-md text-sm opacity-70">{t("app.placeholder")}</p>
    </main>
  );
}
