import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import "../../../i18n";
import type { CampaignProgressEvent, CampaignRowDto } from "../../../ipc";
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
    acceptedPerSecond: 42.5,
    done: false,
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

  render(<CampaignPanel campaign={aRow()} progress={undefined} {...handlers} {...props} />);

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
   * **Where the throughput comes from.** The reading carries the campaign's own
   * rate, measured in the backend from its acceptances. The panel shows that
   * and nothing else — a figure taken from `metrics:tick` would count every
   * submission on the link, including a unit send made beside the campaign
   * (spec §15.3, ADR 0015).
   */
  it("shows the throughput the campaign itself reported", () => {
    panel({ progress: aReading({ accepted: 48, acceptedPerSecond: 42.5 }) });

    expect(screen.getByText("42.5")).toBeInTheDocument();
  });

  /**
   * No reading is not a throughput of zero: a campaign nobody has heard from
   * and one that has stalled must not read the same.
   */
  it("shows no throughput at all before the first reading", () => {
    panel({ progress: undefined });

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
   * **The blocker, seen from the screen.** A paused campaign keeps receiving
   * readings four times a second, and each of them carries `PAUSED` now that
   * the backend reads the control. The resume button has to survive them —
   * when the readings said `RUNNING`, it appeared for under 250 ms and the
   * operator was left with cancellation as the only way out of a pause.
   */
  it("keeps offering Resume while a paused campaign goes on reporting", async () => {
    const handlers = panel({
      campaign: aRow({ status: "PAUSED", live: true }),
      progress: aReading({ status: "PAUSED", processed: 120 }),
    });

    expect(screen.queryByRole("button", { name: "Mettre en pause" })).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Reprendre" }));

    expect(handlers.onResume).toHaveBeenCalledTimes(1);
  });

  /**
   * A campaign the operator has just cancelled is still **live** — it drains
   * its queue before its task returns — and its readings carry `CANCELLED`
   * from that moment. There is nothing left to offer, and offering *Mettre en
   * pause* would offer to suspend something already stopping.
   */
  it("offers nothing on a campaign that is draining after a cancellation", () => {
    panel({
      campaign: aRow({ status: "CANCELLED", live: true }),
      progress: aReading({ status: "CANCELLED" }),
    });

    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });

  /**
   * `campaigns.delivered_count` is fed by nothing in this workspace. Rendered
   * beside five exact counters it read as "the message centre took 200 000 and
   * none arrived", which is an incident opened against an operator who did
   * nothing wrong.
   */
  it("shows no delivery counter, because nothing feeds one yet", () => {
    panel({
      campaign: aRow({ sent: 200, delivered: 0 }),
      progress: aReading({ accepted: 200 }),
    });

    expect(screen.queryByText("Délivrés")).not.toBeInTheDocument();
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
