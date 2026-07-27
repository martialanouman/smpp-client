import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type {
  IpcOutcome,
  LogFilterInput,
  LogPageDto,
  LogRowDto,
  OrphanPageDto,
  PduPageDto,
} from "../../ipc";

import "../../i18n";
import i18n from "../../i18n";
import { NO_FILTER, useLogs } from "../../store/logs";
import { LogsView } from "./LogsView";

const logsQuery =
  vi.fn<
    (
      filter: LogFilterInput,
      cursor: string | null,
      limit: number | null,
    ) => Promise<IpcOutcome<LogPageDto>>
  >();
const logsOrphans = vi.fn<() => Promise<IpcOutcome<OrphanPageDto>>>();
const logsPdus = vi.fn<() => Promise<IpcOutcome<PduPageDto>>>();
const logsSetPduLogging = vi.fn<(enabled: boolean) => Promise<IpcOutcome<boolean>>>();

// The event subscription is mocked to a no-op: there is no Tauri backend under
// jsdom and the real one rejects. What the screen does with a *payload* is
// checked through the store, which is where a payload lands.
vi.mock("../../ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../ipc")>()),
  logsQuery: (filter: LogFilterInput, cursor: string | null, limit: number | null) =>
    logsQuery(filter, cursor, limit),
  logsOrphans: () => logsOrphans(),
  logsPdus: () => logsPdus(),
  logsSetPduLogging: (enabled: boolean) => logsSetPduLogging(enabled),
  onMessageUpdate: () => Promise.resolve(() => undefined),
}));

function aRow(index: number, overrides: Partial<LogRowDto> = {}): LogRowDto {
  return {
    clientMessageId: `1111${String(index).padStart(4, "0")}-2222-4333-8444-555555555555`,
    sessionId: null,
    campaignId: null,
    smscMessageId: `SMSC-${String(index)}`,
    sourceAddr: "SHINOBI",
    destAddr: `+2250102${String(index).padStart(6, "0")}`,
    segments: 1,
    text: "Bonjour",
    state: "DELIVERED",
    commandStatus: 0,
    commandStatusSymbol: "ESME_ROK",
    dlrStat: "DELIVRD",
    dlrErr: "000",
    attempts: 1,
    createdAt: "2026-07-26T10:00:00Z",
    sentAt: "2026-07-26T10:00:01Z",
    respAt: "2026-07-26T10:00:02Z",
    dlrAt: "2026-07-26T10:00:30Z",
    ...overrides,
  };
}

/** A page of `count` rows, announcing `total` behind them. */
function aPage(count: number, total: number, next: string | null): LogPageDto {
  return {
    rows: Array.from({ length: count }, (_unused, index) => aRow(index)),
    next,
    total,
  };
}

/**
 * Gives jsdom a viewport.
 *
 * jsdom implements no layout: every element reports a `clientHeight` of zero.
 * The window the table renders is derived from that height, so without this
 * *every* assertion below would fail for the same reason and none of them would
 * be about the screen.
 *
 * This stubs the browser's layout engine, not the code under test: `rowWindow`
 * still decides which rows fall inside the window, and its arithmetic has its
 * own unit tests over numbers in `rowWindow.test.ts`.
 */
function giveJsdomAViewport() {
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get: () => 600,
  });
}

describe("LogsView", () => {
  beforeEach(async () => {
    giveJsdomAViewport();

    useLogs.setState({
      tab: "messages",
      filter: NO_FILTER,
      rows: [],
      total: 0,
      orphans: [],
      orphanTotal: 0,
      pdus: [],
      pduLogging: false,
      cursor: null,
      exhausted: false,
      loading: false,
      failure: null,
      selected: null,
    });

    logsQuery.mockReset();
    logsOrphans.mockReset();
    logsPdus.mockReset();
    logsSetPduLogging.mockReset();

    logsQuery.mockResolvedValue({ ok: true, value: aPage(3, 3, null) });
    logsOrphans.mockResolvedValue({
      ok: true,
      value: {
        rows: [
          {
            id: "7",
            sessionId: null,
            smscMessageId: "STRANGER",
            reason: "UNKNOWN_ID",
            dlrStat: "DELIVRD",
            dlrErr: "000",
            raw: "id:STRANGER stat:DELIVRD",
            receivedAt: "2026-07-26T12:00:00Z",
          },
        ],
        next: null,
        total: 1,
      },
    });
    logsPdus.mockResolvedValue({ ok: true, value: { rows: [], next: null, enabled: false } });
    logsSetPduLogging.mockResolvedValue({ ok: true, value: true });

    await i18n.changeLanguage("fr");
  });

  it("loads the first page and shows its rows", async () => {
    render(<LogsView />);

    expect(await screen.findByText("+2250102000000")).toBeInTheDocument();
    expect(logsQuery).toHaveBeenCalledWith(NO_FILTER, null, 100);
  });

  /**
   * **CA-008-07, the frontend half.** The backend reports a total of 200 000
   * and hands over one page; the table must render a window, not the total.
   *
   * The assertion is on the number of `<tr>` in the document: a table that
   * rendered its rows without windowing would produce one per loaded row. The
   * scrollbar's height is checked separately, because it is what a virtualised
   * table gets wrong when it derives it from what is loaded rather than from
   * the total the backend reported.
   */
  it("renders a window rather than every loaded row", async () => {
    logsQuery.mockResolvedValue({ ok: true, value: aPage(200, 200_000, "200") });

    render(<LogsView />);

    await screen.findByText("+2250102000000");

    const rendered = document.querySelectorAll("tbody tr");

    // A 600-pixel viewport at 36 pixels a row is seventeen rows, plus eight of
    // overscan on each side: well under the two hundred that are loaded, and
    // under the two hundred thousand that exist.
    expect(rendered.length).toBeLessThan(40);
    expect(rendered.length).toBeGreaterThan(0);

    // The scrollbar is sized from the rows LOADED, which is what lets the
    // operator scroll into the pages that follow; the count beside the tabs is
    // the total the backend reported.
    const body = document.querySelector("tbody");
    expect(body).toHaveStyle({ height: "7200px" });
    expect(screen.getByText("200000 ligne(s)")).toBeInTheDocument();
  });

  /**
   * Combined filters go to the backend **as one filter**, and the rows of the
   * previous filter are discarded rather than kept while the new page arrives:
   * showing them would show exactly what the operator has just excluded.
   */
  it("sends every criterion at once and clears the previous rows", async () => {
    const user = userEvent.setup();
    render(<LogsView />);

    await screen.findByText("+2250102000000");

    await user.type(screen.getByLabelText(/Recherche/u), "promotion");
    await user.type(screen.getByLabelText(/Préfixe destinataire/u), "+225");
    await user.type(screen.getByLabelText(/Code d'erreur/u), "058");
    await user.selectOptions(screen.getByLabelText(/^État$/u), "FAILED");

    logsQuery.mockResolvedValue({ ok: true, value: aPage(0, 0, null) });
    await user.click(screen.getByRole("button", { name: /Filtrer/u }));

    await waitFor(() => {
      expect(logsQuery).toHaveBeenLastCalledWith(
        expect.objectContaining({
          search: "promotion",
          destPrefix: "+225",
          dlrErr: "058",
          state: "FAILED",
        }),
        null,
        100,
      );
    });

    expect(screen.queryByText("+2250102000000")).not.toBeInTheDocument();
  });

  /** CA-008-04 — the orphaned receipts are reachable and show their reason. */
  it("shows the orphaned receipts on their own tab", async () => {
    const user = userEvent.setup();
    render(<LogsView />);

    await screen.findByText("+2250102000000");
    await user.click(screen.getByRole("button", { name: /Accusés orphelins/u }));

    expect(await screen.findByText("STRANGER")).toBeInTheDocument();
    expect(screen.getByText("UNKNOWN_ID")).toBeInTheDocument();
  });

  /** Clicking a row opens the detail panel on that row. */
  it("opens the detail panel for the row that was clicked", async () => {
    const user = userEvent.setup();
    render(<LogsView />);

    await user.click(await screen.findByText("+2250102000001"));

    const panel = screen.getByLabelText(/Détail de la ligne/u);

    expect(panel).toHaveTextContent("SMSC-1");
    expect(panel).toHaveTextContent("DELIVRD");
  });

  /**
   * **CA-008-09** — the PDU tab says *why* it is empty. "Nothing recorded" and
   * "recording is off" look identical on screen, and a panel that showed the
   * same emptiness for both would send an operator hunting a switch.
   */
  it("says that PDU recording is off rather than showing an unexplained empty table", async () => {
    const user = userEvent.setup();
    render(<LogsView />);

    await screen.findByText("+2250102000000");
    await user.click(screen.getByRole("button", { name: /^PDU$/u }));

    expect(await screen.findByText(/enregistrement des PDU est désactivé/iu)).toBeInTheDocument();
    expect(screen.getByLabelText(/Enregistrer les PDU/u)).not.toBeChecked();
  });

  /** And turning it on goes through the backend, which reports what is in force. */
  it("turns PDU recording on through the backend", async () => {
    const user = userEvent.setup();
    render(<LogsView />);

    await screen.findByText("+2250102000000");
    await user.click(screen.getByRole("button", { name: /^PDU$/u }));

    logsPdus.mockResolvedValue({ ok: true, value: { rows: [], next: null, enabled: true } });
    await user.click(await screen.findByLabelText(/Enregistrer les PDU/u));

    await waitFor(() => {
      expect(logsSetPduLogging).toHaveBeenCalledWith(true);
    });
    expect(await screen.findByLabelText(/Enregistrer les PDU/u)).toBeChecked();
  });

  /** A backend refusal is shown, translated from its stable code. */
  it("reports a rejected filter with its translated code", async () => {
    logsQuery.mockResolvedValue({
      ok: false,
      failure: {
        kind: "backend",
        error: { code: "LOGS_INVALID_FILTER", message: "bad filter", details: null },
      },
    });

    render(<LogsView />);

    expect(await screen.findByRole("alert")).toHaveTextContent(/critère de filtre/iu);
  });
});
