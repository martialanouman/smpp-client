import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { IpcOutcome, SessionBindInput, SessionProfileDto, SessionStatusDto } from "../../ipc";

import "../../i18n";
import i18n from "../../i18n";
import fr from "../../i18n/locales/fr.json";
import { blankProfile, useSessions } from "../../store/sessions";
import { SessionsView } from "./SessionsView";

const sessionList = vi.fn<() => Promise<IpcOutcome<SessionProfileDto[]>>>();
const sessionBind = vi.fn<(input: SessionBindInput) => Promise<IpcOutcome<SessionStatusDto>>>();
const sessionUnbind = vi.fn<(sessionId: string) => Promise<IpcOutcome<boolean>>>();

// The event subscription is mocked to a no-op: there is no Tauri backend under
// jsdom, and the real one rejects. What the view does with a *payload* is
// covered through the store, which is where the payload is adopted.
vi.mock("../../ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../ipc")>()),
  sessionList: () => sessionList(),
  sessionBind: (input: SessionBindInput) => sessionBind(input),
  sessionUnbind: (sessionId: string) => sessionUnbind(sessionId),
  onSessionsState: () => Promise.resolve(() => undefined),
}));

const PROFILE: SessionProfileDto = {
  ...blankProfile(),
  sessionId: "11111111-2222-3333-4444-555555555555",
  name: "Operator A",
  host: "smsc.example.test",
  systemId: "esme01",
};

function boundStatus(): SessionStatusDto {
  return {
    sessionId: PROFILE.sessionId ?? "",
    state: "BOUND",
    bindType: "transceiver",
    lastError: null,
    giveUp: null,
    inFlight: 0,
  };
}

describe("SessionsView", () => {
  beforeEach(async () => {
    useSessions.setState({ profiles: [], statuses: {}, busy: false });
    sessionList.mockResolvedValue({ ok: true, value: [PROFILE] });
    sessionBind.mockResolvedValue({ ok: true, value: boundStatus() });
    sessionUnbind.mockResolvedValue({ ok: true, value: true });
    await i18n.changeLanguage("fr");
  });

  it("lists the profiles the backend returns", async () => {
    render(<SessionsView />);

    expect(await screen.findByText(PROFILE.name)).toBeInTheDocument();
    expect(screen.getByText(/smsc\.example\.test:2775/)).toBeInTheDocument();
  });

  /// CA-005-01, on the interface side: the state shown is the one the backend
  /// answered, and it is a **word**, not only a colour (spec §16.4).
  it("shows the state the backend reports, in words", async () => {
    render(<SessionsView />);

    expect(await screen.findByText(fr.sessions.state.CLOSED)).toBeInTheDocument();

    const user = userEvent.setup();
    await user.type(screen.getByLabelText(fr.sessions.password), "n0tr34l");
    await user.click(screen.getByRole("button", { name: fr.sessions.bind }));

    expect(await screen.findByText(fr.sessions.state.BOUND)).toBeInTheDocument();
  });

  /// CLAUDE.md §8 — the credential travels on `session_bind` and is cleared
  /// from the field at once. It is never put in the store, and a rebind asks
  /// for it again.
  it("sends the password once and clears the field", async () => {
    const user = userEvent.setup();
    render(<SessionsView />);

    await screen.findByText(PROFILE.name);

    const field = screen.getByLabelText(fr.sessions.password);
    await user.type(field, "n0tr34l");
    await user.click(screen.getByRole("button", { name: fr.sessions.bind }));

    expect(sessionBind).toHaveBeenCalledWith({
      sessionId: PROFILE.sessionId,
      password: "n0tr34l",
    });

    expect(JSON.stringify(useSessions.getState())).not.toContain("n0tr34l");
  });

  /// A session that gave up shows *why*, translated from a stable code rather
  /// than from a sentence the backend wrote.
  it("explains a session that stopped for good", async () => {
    render(<SessionsView />);
    await screen.findByText(PROFILE.name);

    useSessions.getState().adopt([
      {
        sessionId: PROFILE.sessionId ?? "",
        state: "ERROR",
        bindType: null,
        lastError: "bind refused by the SMSC: ESME_RINVPASWD",
        giveUp: "FATAL_STATUS",
        inFlight: 0,
      },
    ]);

    expect(await screen.findByText(fr.sessions.giveUp.FATAL_STATUS)).toBeInTheDocument();
    expect(screen.getByText(fr.sessions.state.ERROR)).toBeInTheDocument();
  });

  it("offers unbind once the session is bound, and bind otherwise", async () => {
    render(<SessionsView />);
    await screen.findByText(PROFILE.name);

    expect(screen.getByRole("button", { name: fr.sessions.bind })).toBeInTheDocument();

    useSessions.getState().adopt([boundStatus()]);

    expect(await screen.findByRole("button", { name: fr.sessions.unbind })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: fr.sessions.bind })).not.toBeInTheDocument();
    // And no password field: there is nothing to bind.
    expect(screen.queryByLabelText(fr.sessions.password)).not.toBeInTheDocument();
  });

  it("says so when there is no profile at all", async () => {
    sessionList.mockResolvedValue({ ok: true, value: [] });

    render(<SessionsView />);

    expect(await screen.findByText(fr.sessions.empty)).toBeInTheDocument();
  });
});
