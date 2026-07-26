import { useTranslation } from "react-i18next";

import { usePreferences } from "../store/preferences";

/**
 * Backend errors, shown as dismissible toasts.
 *
 * The stable `code` is displayed next to the message: the message is meant for
 * a human and may be reworded, the code is what a bug report can be searched
 * on and what milestone 015 will map to a remediation.
 *
 * `role="region"` with `aria-live="polite"` rather than `role="alert"`: an
 * alert interrupts a screen reader mid-sentence, which is disproportionate for
 * a configuration error the user just caused.
 */
export function Notifications() {
  const { t } = useTranslation();
  const notifications = usePreferences((state) => state.notifications);
  const dismiss = usePreferences((state) => state.dismiss);

  if (notifications.length === 0) {
    return null;
  }

  return (
    <div
      role="region"
      aria-label={t("notification.region")}
      aria-live="polite"
      className="fixed right-4 bottom-4 flex w-80 flex-col gap-2"
    >
      {notifications.map(({ id, code, message }) => (
        <div
          key={id}
          className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] p-3 shadow-lg"
        >
          <div className="flex items-start justify-between gap-2">
            {/* `message` is the backend's fixed English sentence, meant for
                logs and bug reports — src-tauri/src/error.rs states it must
                never be shown raw. The user reads the translation of `code`;
                a transport failure has none, so it falls back to a generic
                sentence rather than leaking a technical string. */}
            <p className="text-sm">
              {code === null ? t("error.unknown") : t(`error.${code}`, { defaultValue: message })}
            </p>
            <button
              type="button"
              aria-label={t("notification.dismiss")}
              onClick={() => dismiss(id)}
              className="shrink-0 rounded px-1 text-sm opacity-60 hover:opacity-100"
            >
              ×
            </button>
          </div>
          {/* The code and the raw message stay visible as the technical line:
              that is what makes a bug report actionable, and it is the only
              place the English sentence belongs. */}
          <p className="mt-1 font-mono text-xs opacity-60">{code ?? message}</p>
        </div>
      ))}
    </div>
  );
}
