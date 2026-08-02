import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import "../../../i18n";
import type { CampaignProgressEvent, CampaignRowDto, MetricsTick } from "../../../ipc";
import { CampaignPanel } from "./CampaignPanel";

function aRow(overrides: Partial<CampaignRowDto> = {}): CampaignRowDto {
  return {
    campaignId: "campaign-1",
    name: "Juillet",
    status: "VALIDATED",
    template: "Bonjour",
    config: null,
    total: 200,
    sent: 0,
    delivered: 0,
    failed: 0,
    live: false,
    createdAt: "2026-08-02T10:00:00Z",
    startedAt: null,
    completedAt: null,
    ...overrides,
  };
}

function aReading(overrides: Partial<CampaignProgressEvent> = {}): CampaignProgressEvent {
  return {
    campaignId: "campaign-1",
    sessionId: "session-1",
    status: "RUNNING",
    total: 200,
    processed: 50,
    accepted: 48,
    failed: 2,
    rejected: 0,
    skipped: 0,
    cancelled: 0,
    retried: 0,
    reemittedUnanswered: 0,
    notJournalled: 0,
    done: false,
    ...overrides,
  };
}

function aTick(overrides: Partial<MetricsTick> = {}): MetricsTick {
  return {
    sessionId: "session-1",
    tps1s: 42.5,
    tps10s: 40,
    tpsAverage: 39,
    tpsPeak: 55,
    targetTps: 100,
    windowSize: 50,
    windowInUse: 12,
    windowOccupancy: 0.24,
    rttMs: 18,
    reconnects: 0,
    uptimeS: 300,
    submitted: 100,
    accepted: 98,
    rejected: 2,
    timedOut: 0,
    throttled: 0,
    backingOff: false,
    adaptivePermille: 1000,
    ...overrides,
  };
}

function panel(props: Partial<Parameters<typeof CampaignPanel>[0]> = {}) {
  const handlers = {
    onStart: vi.fn(),
    onPause: vi.fn(),
    onResume: vi.fn(),
    onCancel: vi.fn(),
  };

  render(
    <CampaignPanel
      campaign={aRow()}
      progress={undefined}
      metrics={undefined}
      {...handlers}
      {...props}
    />,
  );

  return handlers;
}

describe("the campaign panel", () => {
  it("draws the progress against the campaign's planned total", () => {
    panel({ progress: aReading() });

    const meter = screen.getByRole("meter");

    expect(meter).toHaveAttribute("aria-valuenow", "25");
    expect(screen.getByText("50 / 200")).toBeInTheDocument();
  });

  /**
   * **Where the throughput comes from.** The reading carries counters and no
   * rate; the figure is the `metrics:tick` of the session it names. A panel
   * that derived one from `accepted` over time would show a different number
   * from the gauges on the Sessions and Dashboard screens.
   */
  it("shows the throughput measured by the session, not one of its own", () => {
    panel({ progress: aReading({ accepted: 48 }), metrics: aTick({ tps1s: 42.5 }) });

    expect(screen.getByText("42.5")).toBeInTheDocument();
  });

  /** No tick yet is not a throughput of zero: a stalled link and an idle one
   * must not read the same. */
  it("shows no throughput at all when the session has not been measured", () => {
    panel({ progress: aReading(), metrics: undefined });

    expect(screen.getByText("—")).toBeInTheDocument();
  });

  it("offers Start on a campaign that has never run", async () => {
    const handlers = panel({ campaign: aRow({ status: "VALIDATED", live: false }) });

    await userEvent.click(screen.getByRole("button", { name: "Démarrer" }));

    expect(handlers.onStart).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "Mettre en pause" })).not.toBeInTheDocument();
  });

  it("offers Pause and Cancel while a campaign is live", async () => {
    const handlers = panel({ campaign: aRow({ status: "RUNNING", live: true }) });

    await userEvent.click(screen.getByRole("button", { name: "Mettre en pause" }));
    await userEvent.click(screen.getByRole("button", { name: "Annuler" }));

    expect(handlers.onPause).toHaveBeenCalledTimes(1);
    expect(handlers.onCancel).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "Démarrer" })).not.toBeInTheDocument();
  });

  it("offers Resume on a campaign paused in this process", async () => {
    const handlers = panel({ campaign: aRow({ status: "PAUSED", live: true }) });

    await userEvent.click(screen.getByRole("button", { name: "Reprendre" }));

    expect(handlers.onResume).toHaveBeenCalledTimes(1);
  });

  /**
   * **The case the status alone cannot express.** A process killed
   * mid-campaign leaves a row reading `RUNNING` with nothing running. Offering
   * Pause there would offer to suspend something that has already stopped;
   * what it needs is Reprendre, which restarts in resuming mode and re-sends
   * nothing already accepted (CA-010-05).
   */
  it("offers Resume, and says so, on a campaign a restart left behind", async () => {
    const handlers = panel({ campaign: aRow({ status: "RUNNING", live: false }) });

    expect(screen.getByText(/interrompue/u)).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Reprendre" }));

    expect(handlers.onResume).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "Mettre en pause" })).not.toBeInTheDocument();
  });

  it("offers nothing on a campaign that has ended", () => {
    for (const status of ["COMPLETED", "CANCELLED", "FAILED"]) {
      const { unmount } = render(
        <CampaignPanel
          campaign={aRow({ status, live: false })}
          progress={undefined}
          metrics={undefined}
          onStart={vi.fn()}
          onPause={vi.fn()}
          onResume={vi.fn()}
          onCancel={vi.fn()}
        />,
      );

      expect(screen.queryAllByRole("button"), status).toHaveLength(0);
      unmount();
    }
  });

  /**
   * The duplicate-risk figure of ADR 0014 is shown rather than buried: under
   * the default arbitration each of those messages may reach its recipient
   * twice, and an operator has to be able to see how many are at stake.
   */
  it("says how many messages may have been sent twice", () => {
    panel({ progress: aReading({ reemittedUnanswered: 5 }) });

    expect(screen.getByText(/5 message\(s\) laissé\(s\) en vol/u)).toBeInTheDocument();
  });

  it("says nothing about duplicates when there are none", () => {
    panel({ progress: aReading({ reemittedUnanswered: 0 }) });

    expect(screen.queryByText(/laissé\(s\) en vol/u)).not.toBeInTheDocument();
  });

  /** Before the first reading, the row's own counters are what there is. */
  it("falls back on the stored counters before the first reading", () => {
    panel({ campaign: aRow({ sent: 120, failed: 5, total: 200 }), progress: undefined });

    expect(screen.getByText("125 / 200")).toBeInTheDocument();
  });
});
