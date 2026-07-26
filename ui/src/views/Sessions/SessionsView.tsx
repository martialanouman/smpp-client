import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { onSessionsState } from "../../ipc";
import type { SessionProfileDto, SessionStatusDto } from "../../ipc";
import { usePreferences } from "../../store/preferences";
import { blankProfile, useSessions } from "../../store/sessions";
import { ProfileForm } from "./ProfileForm";

/** The state a profile with no live session is in. */
const CLOSED = "CLOSED";

/**
 * The colour of a state badge.
 *
 * A colour and a **word**, never a colour alone: spec §16.4 asks for
 * accessibility, and a red dot beside a green dot is nothing at all to eight
 * per cent of men.
 */
function badgeClass(state: string): string {
  switch (state) {
    case "BOUND":
      return "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300";
    case "CONNECTING":
    case "BINDING":
    case "RECONNECT":
      return "bg-amber-500/15 text-amber-700 dark:text-amber-300";
    case "ERROR":
      return "bg-red-500/15 text-red-700 dark:text-red-300";
    default:
      return "bg-[var(--shinobi-hover)] opacity-80";
  }
}

interface RowProps {
  readonly profile: SessionProfileDto;
  readonly status: SessionStatusDto | undefined;
}

/** One profile, its state, and what can be done to it. */
function SessionRow({ profile, status }: RowProps) {
  const { t } = useTranslation();
  const [password, setPassword] = useState("");
  const [editing, setEditing] = useState(false);
  const bind = useSessions((state) => state.bind);
  const unbind = useSessions((state) => state.unbind);
  const remove = useSessions((state) => state.remove);
  const busy = useSessions((state) => state.busy);

  const sessionId = profile.sessionId ?? "";
  const state = status?.state ?? CLOSED;
  const live = state !== CLOSED && state !== "UNBOUND" && state !== "ERROR";

  return (
    <li className="rounded-md border border-[var(--shinobi-border)] p-4">
      <div className="flex flex-wrap items-center gap-3">
        <span className="font-medium">{profile.name}</span>

        <span className={`rounded px-2 py-0.5 text-xs font-medium ${badgeClass(state)}`}>
          {t(`sessions.state.${state}`)}
        </span>

        <span className="text-sm opacity-70">
          {profile.host}:{profile.port} · {t(`sessions.bindType.${profile.bindType}`)} ·{" "}
          {profile.interfaceVersion}
        </span>

        <span className="ml-auto text-sm opacity-70">
          {t("sessions.inFlight", { count: status?.inFlight ?? 0 })}
        </span>
      </div>

      {status?.giveUp ? (
        <p className="mt-2 text-sm text-red-700 dark:text-red-300">
          {t(`sessions.giveUp.${status.giveUp}`)}
        </p>
      ) : null}

      {status?.lastError ? (
        <p className="mt-1 font-mono text-xs opacity-70">{status.lastError}</p>
      ) : null}

      <div className="mt-3 flex flex-wrap items-end gap-2">
        {live ? null : (
          <label className="flex flex-col gap-1">
            <span className="text-xs font-medium">{t("sessions.password")}</span>
            <input
              type="password"
              autoComplete="off"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              className="rounded-md border border-[var(--shinobi-border)] bg-[var(--shinobi-surface)] px-3 py-2 text-sm"
            />
          </label>
        )}

        {live ? (
          <button
            type="button"
            disabled={busy}
            onClick={() => void unbind(sessionId)}
            className="rounded-md border border-[var(--shinobi-border)] px-3 py-2 text-sm hover:bg-[var(--shinobi-hover)] disabled:opacity-50"
          >
            {t("sessions.unbind")}
          </button>
        ) : (
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              // The password leaves with the call and is cleared at once: it
              // is not kept in the store, and it must not linger in a field
              // either (CLAUDE.md §8).
              void bind(sessionId, password);
              setPassword("");
            }}
            className="rounded-md bg-[var(--shinobi-accent)] px-3 py-2 text-sm font-medium disabled:opacity-50"
          >
            {t("sessions.bind")}
          </button>
        )}

        <button
          type="button"
          disabled={busy}
          onClick={() => setEditing((open) => !open)}
          className="rounded-md border border-[var(--shinobi-border)] px-3 py-2 text-sm hover:bg-[var(--shinobi-hover)] disabled:opacity-50"
        >
          {t(editing ? "sessions.close" : "sessions.edit")}
        </button>

        <button
          type="button"
          disabled={busy}
          onClick={() => void remove(sessionId)}
          className="rounded-md border border-[var(--shinobi-border)] px-3 py-2 text-sm hover:bg-[var(--shinobi-hover)] disabled:opacity-50"
        >
          {t("sessions.delete")}
        </button>
      </div>

      {editing ? (
        <div className="mt-4 border-t border-[var(--shinobi-border)] pt-4">
          <ProfileForm initial={profile} onDone={() => setEditing(false)} />
        </div>
      ) : null}
    </li>
  );
}

/**
 * Connection profiles, their live state, and bind/unbind.
 *
 * The state comes from `sessions:state` and from the answer to a command, and
 * from nowhere else: a screen that guessed "it must be bound by now" would
 * disagree with the session it is meant to be showing.
 */
export function SessionsView() {
  const { t } = useTranslation();
  const profiles = useSessions((state) => state.profiles);
  const statuses = useSessions((state) => state.statuses);
  const refresh = useSessions((state) => state.refresh);
  const adopt = useSessions((state) => state.adopt);
  const notify = usePreferences((state) => state.notify);
  const [creating, setCreating] = useState(false);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    // The listener is registered once and torn down on unmount. Without the
    // teardown a remount would stack listeners, and every state change would
    // be adopted as many times as the screen had been opened.
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    onSessionsState((payload) => adopt(payload.sessions))
      .then((stop) => {
        if (cancelled) {
          stop();
        } else {
          unlisten = stop;
        }
      })
      .catch((cause: unknown) => {
        // A failed subscription must not sink the screen. `bridge.ts` treats
        // `error:notify` the same way, and for the same reason: the profiles
        // and the commands still work, only the live state stops updating —
        // and a thrown promise here would take the whole view down with it.
        notify({
          code: null,
          message: cause instanceof Error ? cause.message : String(cause),
        });
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [adopt, notify]);

  return (
    <div className="flex max-w-3xl flex-col gap-6">
      <p className="text-sm opacity-70">{t("screen.sessions")}</p>

      {profiles.length === 0 ? (
        <p className="rounded-md border border-dashed border-[var(--shinobi-border)] p-6 text-sm">
          {t("sessions.empty")}
        </p>
      ) : (
        <ul className="flex flex-col gap-3">
          {profiles.map((profile) => (
            <SessionRow
              key={profile.sessionId ?? profile.name}
              profile={profile}
              status={profile.sessionId ? statuses[profile.sessionId] : undefined}
            />
          ))}
        </ul>
      )}

      {creating ? (
        <div className="rounded-md border border-[var(--shinobi-border)] p-4">
          <h2 className="mb-3 text-sm font-medium">{t("sessions.newProfile")}</h2>
          <ProfileForm initial={blankProfile()} onDone={() => setCreating(false)} />
        </div>
      ) : (
        <button
          type="button"
          onClick={() => setCreating(true)}
          className="self-start rounded-md bg-[var(--shinobi-accent)] px-3 py-2 text-sm font-medium"
        >
          {t("sessions.newProfile")}
        </button>
      )}
    </div>
  );
}
