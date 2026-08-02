import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import "../../../i18n";
import type { CampaignProgressEvent, CampaignRowDto } from "../../../ipc";
import { useCampaigns } from "../../../store/campaign";
import { CampaignPanel } from "./CampaignPanel";

vi.mock("../../../store/bridge", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../store/bridge")>()),
  report: () => undefined,
}));

/** The backend's cadence, in milliseconds — `CAMPAIGN_PROGRESS_INTERVAL`. */
const INTERVAL = 250;

function aRow(overrides: Partial<CampaignRowDto> = {}): CampaignRowDto {
  return {
    campaignId: "campaign-1",
    name: "Juillet",
    status: "RUNNING",
    template: "Bonjour",
    config: null,
    total: 200_000,
    sent: 0,
    delivered: 0,
    failed: 0,
    live: true,
    createdAt: "2026-08-02T10:00:00Z",
    startedAt: "2026-08-02T10:01:00Z",
    completedAt: null,
    ...overrides,
  };
}

function aReading(overrides: Partial<CampaignProgressEvent> = {}): CampaignProgressEvent {
  return {
    campaignId: "campaign-1",
    sessionId: "session-1",
    status: "RUNNING",
    total: 200_000,
    processed: 1_000,
    accepted: 1_000,
    failed: 0,
    rejected: 0,
    skipped: 0,
    cancelled: 0,
    retried: 0,
    reemittedUnanswered: 0,
    notJournalled: 0,
    acceptedPerSecond: 120,
    done: false,
    ...overrides,
  };
}

/** The panel, driven by the store the way the screen drives it. */
function Screen() {
  const rows = useCampaigns((state) => state.rows);
  const progress = useCampaigns((state) => state.progress);
  const campaign = rows[0];

  if (campaign === undefined) return null;

  return (
    <CampaignPanel
      campaign={campaign}
      progress={progress[campaign.campaignId]}
      onStart={() => undefined}
      onPause={() => undefined}
      onResume={() => undefined}
      onCancel={() => undefined}
    />
  );
}

/**
 * **The blocker, end to end across the store and the screen.**
 *
 * Pausing a campaign of two hundred thousand recipients used to be a trap the
 * operator could not get out of. `campaign_pause` wrote `PAUSED` and the button
 * became *Reprendre*; the progress sampler then published a **hard-wired**
 * `RUNNING` 250 ms later, the store moved the row's status onto it, and the
 * panel put *Mettre en pause* back. Clicking that called `campaign_pause` again
 * — `PAUSED → PAUSED` is a legal no-op — so the resume button existed for under
 * a quarter of a second at a time and cancellation was the only way out.
 *
 * The sampler now reads the control, so the readings that keep arriving carry
 * `PAUSED`. This drives ten of them — two and a half seconds of a live campaign
 * at the backend's real cadence — through the real store and the real component,
 * and the button has to survive every one.
 */
describe("a paused campaign that goes on reporting", () => {
  beforeEach(() => {
    useCampaigns.setState({ rows: [], progress: {}, loading: false, creating: false });
  });

  it("keeps its resume button for as long as the readings keep coming", () => {
    vi.useFakeTimers();

    useCampaigns.setState({ rows: [aRow({ status: "PAUSED" })] });
    render(<Screen />);

    expect(screen.getByRole("button", { name: "Reprendre" })).toBeInTheDocument();

    for (let tick = 0; tick < 10; tick += 1) {
      act(() => {
        useCampaigns
          .getState()
          .applyProgress(aReading({ status: "PAUSED", processed: 1_000 + tick }));
        vi.advanceTimersByTime(INTERVAL);
      });

      expect(
        screen.queryByRole("button", { name: "Mettre en pause" }),
        `the pause button came back after ${(tick + 1) * INTERVAL} ms`,
      ).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Reprendre" })).toBeInTheDocument();
    }

    vi.useRealTimers();
  });

  /** And the counters do keep moving while it is paused — the in-flight window
   * drains (spec §10.3), so the screen is not frozen, only the button is
   * stable. */
  it("goes on showing the counters move while it is paused", () => {
    useCampaigns.setState({ rows: [aRow({ status: "PAUSED" })] });
    render(<Screen />);

    act(() => {
      useCampaigns.getState().applyProgress(aReading({ status: "PAUSED", processed: 1_200 }));
    });

    expect(screen.getByText("1200 / 200000")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reprendre" })).toBeInTheDocument();
  });
});
