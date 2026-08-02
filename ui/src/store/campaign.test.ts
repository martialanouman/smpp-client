import { beforeEach, describe, expect, it, vi } from "vitest";

import type { CampaignProgressEvent, CampaignRowDto } from "../ipc";
import { useCampaigns } from "./campaign";

const notify = vi.fn();
const campaignList = vi.fn();
const campaignCreate = vi.fn();
const campaignStart = vi.fn();
const campaignPause = vi.fn();
const campaignResume = vi.fn();
const campaignCancel = vi.fn();

vi.mock("./bridge", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./bridge")>()),
  report: (failure: unknown) => notify(failure) as unknown,
}));

vi.mock("../ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../ipc")>()),
  campaignList: (...args: unknown[]) => campaignList(...args) as unknown,
  campaignCreate: (...args: unknown[]) => campaignCreate(...args) as unknown,
  campaignStart: (...args: unknown[]) => campaignStart(...args) as unknown,
  campaignPause: (...args: unknown[]) => campaignPause(...args) as unknown,
  campaignResume: (...args: unknown[]) => campaignResume(...args) as unknown,
  campaignCancel: (...args: unknown[]) => campaignCancel(...args) as unknown,
}));

function aRow(campaignId: string, overrides: Partial<CampaignRowDto> = {}): CampaignRowDto {
  return {
    campaignId,
    name: `campaign ${campaignId}`,
    status: "VALIDATED",
    template: "Bonjour {{prenom}}",
    config: null,
    total: 100,
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

function aReading(
  campaignId: string,
  overrides: Partial<CampaignProgressEvent> = {},
): CampaignProgressEvent {
  return {
    campaignId,
    sessionId: "session-1",
    status: "RUNNING",
    total: 100,
    processed: 10,
    accepted: 9,
    failed: 1,
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

describe("the campaign store", () => {
  beforeEach(() => {
    notify.mockReset();
    campaignList.mockReset().mockResolvedValue({ ok: true, value: { rows: [], next: null } });
    campaignCreate.mockReset().mockResolvedValue({ ok: true, value: "new-id" });
    campaignStart.mockReset().mockResolvedValue({ ok: true, value: null });
    campaignPause.mockReset().mockResolvedValue({ ok: true, value: null });
    campaignResume.mockReset().mockResolvedValue({ ok: true, value: null });
    campaignCancel.mockReset().mockResolvedValue({ ok: true, value: null });

    useCampaigns.setState({ rows: [], progress: {}, loading: false, creating: false });
  });

  it("loads the campaign list", async () => {
    campaignList.mockResolvedValue({
      ok: true,
      value: { rows: [aRow("a"), aRow("b")], next: null },
    });

    await useCampaigns.getState().reload();

    expect(useCampaigns.getState().rows.map((row) => row.campaignId)).toEqual(["a", "b"]);
  });

  it("reports a failed list rather than blanking the table", async () => {
    useCampaigns.setState({ rows: [aRow("a")] });
    campaignList.mockResolvedValue({
      ok: false,
      failure: { kind: "backend", error: { code: "CAMPAIGN_STORAGE", message: "", details: null } },
    });

    await useCampaigns.getState().reload();

    expect(notify).toHaveBeenCalledTimes(1);
    expect(useCampaigns.getState().rows.map((row) => row.campaignId)).toEqual(["a"]);
  });

  /**
   * Progress is kept per campaign, not as a single "current" reading: several
   * campaigns can run at once, and a single slot would make the last event to
   * arrive overwrite the bar of a different campaign.
   */
  it("keeps one reading per campaign", () => {
    useCampaigns.getState().applyProgress(aReading("a", { accepted: 5 }));
    useCampaigns.getState().applyProgress(aReading("b", { accepted: 50 }));

    expect(useCampaigns.getState().progress["a"]?.accepted).toBe(5);
    expect(useCampaigns.getState().progress["b"]?.accepted).toBe(50);
  });

  /**
   * The reading carries the campaign's status, and it arrives four times a
   * second while the list is reloaded only on a command. A row that kept the
   * status it was fetched with would show `VALIDATED` under a bar at 60 %.
   */
  it("moves the row's status and counters with the reading", () => {
    useCampaigns.setState({ rows: [aRow("a", { status: "VALIDATED" })] });

    useCampaigns.getState().applyProgress(aReading("a", { status: "RUNNING", accepted: 9 }));

    const row = useCampaigns.getState().rows[0];

    expect(row?.status).toBe("RUNNING");
    expect(row?.sent).toBe(9);
    expect(row?.live).toBe(true);
  });

  /**
   * **The final reading is the one that matters.** It carries the terminal
   * status, and the row has to stop claiming to be live — otherwise the screen
   * offers Pause and Cancel on a campaign that has finished.
   */
  it("marks the row as no longer live on the final reading", () => {
    useCampaigns.setState({ rows: [aRow("a", { status: "RUNNING", live: true })] });

    useCampaigns
      .getState()
      .applyProgress(aReading("a", { status: "COMPLETED", accepted: 100, done: true }));

    const row = useCampaigns.getState().rows[0];

    expect(row?.status).toBe("COMPLETED");
    expect(row?.live).toBe(false);
    expect(useCampaigns.getState().progress["a"]?.done).toBe(true);
  });

  /** A reading for a campaign the list does not hold must not invent a row. */
  it("ignores a reading for a campaign it does not know", () => {
    useCampaigns.setState({ rows: [aRow("a")] });

    useCampaigns.getState().applyProgress(aReading("unknown"));

    expect(useCampaigns.getState().rows).toHaveLength(1);
    expect(useCampaigns.getState().progress["unknown"]).toBeDefined();
  });

  it("reloads the list after creating a campaign", async () => {
    const created = await useCampaigns.getState().create({
      name: "Juillet",
      template: "Bonjour",
      config: null as never,
    });

    expect(created).toBe("new-id");
    expect(campaignList).toHaveBeenCalledTimes(1);
  });

  it("reports a refused creation and creates nothing", async () => {
    campaignCreate.mockResolvedValue({
      ok: false,
      failure: {
        kind: "backend",
        error: { code: "CAMPAIGN_NO_RECIPIENTS", message: "", details: null },
      },
    });

    const created = await useCampaigns.getState().create({
      name: "Juillet",
      template: "Bonjour",
      config: null as never,
    });

    expect(created).toBeNull();
    expect(notify).toHaveBeenCalledTimes(1);
    expect(campaignList).not.toHaveBeenCalled();
  });

  it("sends each control to its command and refreshes the list", async () => {
    await useCampaigns.getState().start("a");
    await useCampaigns.getState().pause("a");
    await useCampaigns.getState().resume("a");
    await useCampaigns.getState().cancel("a");

    expect(campaignStart).toHaveBeenCalledWith("a");
    expect(campaignPause).toHaveBeenCalledWith("a");
    expect(campaignResume).toHaveBeenCalledWith("a");
    expect(campaignCancel).toHaveBeenCalledWith("a");
    expect(campaignList).toHaveBeenCalledTimes(4);
  });

  /**
   * A refused control is shown. A start that came back
   * `CAMPAIGN_SESSION_NOT_BOUND` and said nothing would leave the operator
   * looking at a button that appears to do nothing at all.
   */
  it("reports a refused control", async () => {
    campaignStart.mockResolvedValue({
      ok: false,
      failure: {
        kind: "backend",
        error: { code: "CAMPAIGN_SESSION_NOT_BOUND", message: "", details: null },
      },
    });

    await useCampaigns.getState().start("a");

    expect(notify).toHaveBeenCalledTimes(1);
  });
});
