import { useTranslation } from "react-i18next";

/**
 * Milestone 000 holding page.
 *
 * It exists only to prove the whole chain stands up — Vite compiles, React
 * mounts, i18n resolves, the Tauri WebView renders. The real application
 * shell (navigation, business views) lands at milestone 001.
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
