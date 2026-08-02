import { create } from "zustand";

import {
  campaignCancel,
  campaignCreate,
  campaignList,
  campaignPause,
  campaignResume,
  campaignStart,
  type CampaignCreateInput,
  type CampaignProgressEvent,
  type CampaignRowDto,
} from "../ipc";
import { report } from "./bridge";

/**
 * State of the Envoi › Campagne tab (spec §10.2, deliverable L-010-08).
 *
 * # Two sources, and neither can replace the other
 *
 * `campaign_list` is the durable truth — every campaign, with the counters that
 * were written to its row. `campaign:progress` is the live one, four readings a
 * second for the campaigns running right now, and it is the only thing that
 * moves between two commands. So a reading updates the row it names rather than
 * sitting beside it: a row that kept the status it was fetched with would read
 * `VALIDATED` under a progress bar at sixty per cent.
 *
 * # Keyed by campaign, never a single "current" reading
 *
 * Several campaigns can run at once. One slot would make the last event to
 * arrive overwrite the bar of a different campaign, which reads as a progress
 * bar jumping backwards.
 *
 * # No throughput is computed here
 *
 * The rate shown beside a campaign comes from the `metrics:tick` of its session
 * — `useMetrics`, milestone 007 — and is not derived from these counters. Four
 * samples a second are enough to draw a bar and not enough to measure a rate,
 * and a second figure for the same thing would disagree with the gauges on the
 * Sessions and Dashboard screens.
 *
 * # The list is walked to its end, not sampled
 *
 * `campaign_list` is paginated and `reload` follows the cursor until there is
 * none. Asking for the first page alone was a ceiling nobody could see past:
 * with sixty campaigns, one of the ten oldest left `RUNNING` by a crash was
 * simply not listed, and neither Reprendre nor Annuler could reach it.
 *
 * Campaigns are units to hundreds — created by hand, one at a time — so the
 * walk is a handful of calls. What needs virtualising is the **messages** of a
 * campaign, and that is the Journaux screen.
 */

/** Campaigns a page holds. Matches the backend's default. */
const PAGE = 50;

/**
 * How many pages one reload will walk.
 *
 * A bound and not a hint: a backend that kept handing back a cursor would spin
 * this loop for ever inside the WebView, holding the whole interface. A
 * thousand campaigns is far past what this screen is for, and stopping short is
 * a visibly incomplete list rather than a frozen application.
 */
const MAX_PAGES = 20;

interface CampaignsState {
  /** Every campaign, oldest first. */
  readonly rows: readonly CampaignRowDto[];
  /** The latest reading of each campaign, keyed by identifier. */
  readonly progress: Readonly<Record<string, CampaignProgressEvent>>;
  /** Whether a list request is in flight. */
  readonly loading: boolean;
  /** Whether a creation is in flight. */
  readonly creating: boolean;

  /** Loads the campaign list. */
  readonly reload: () => Promise<void>;
  /** Creates a campaign and refreshes the list. Resolves to its identifier. */
  readonly create: (input: CampaignCreateInput) => Promise<string | null>;
  /** Starts a campaign. */
  readonly start: (campaignId: string) => Promise<void>;
  /** Suspends the feeding of a running campaign. */
  readonly pause: (campaignId: string) => Promise<void>;
  /** Resumes a paused campaign, or picks up one a restart left behind. */
  readonly resume: (campaignId: string) => Promise<void>;
  /** Stops a campaign for good. */
  readonly cancel: (campaignId: string) => Promise<void>;
  /** Records one `campaign:progress` reading. */
  readonly applyProgress: (reading: CampaignProgressEvent) => void;
}

export const useCampaigns = create<CampaignsState>((set, get) => ({
  rows: [],
  progress: {},
  loading: false,
  creating: false,

  reload: async () => {
    set({ loading: true });

    const collected: CampaignRowDto[] = [];
    let cursor: string | null = null;

    // The WHOLE list, page after page. Asking for the first page only was a
    // silent ceiling: with sixty campaigns, one of the ten oldest left
    // `RUNNING` by a crash was never listed, and neither Reprendre nor Annuler
    // could ever reach it. Campaigns are units to hundreds — created by hand,
    // one at a time — so the walk is short; what needs virtualising is the
    // *messages* of a campaign, and that is the Journaux screen.
    for (let page = 0; page < MAX_PAGES; page += 1) {
      const outcome: Awaited<ReturnType<typeof campaignList>> = await campaignList(cursor, PAGE);

      if (!outcome.ok) {
        // The pages already read are kept, and so are the rows held before
        // this call: a store that stopped answering is a reason to show what
        // is known with an error beside it, not to blank a screen into
        // something that reads as "you have no campaigns".
        report(outcome.failure);

        if (collected.length > 0) set({ rows: collected });

        set({ loading: false });

        return;
      }

      collected.push(...outcome.value.rows);
      cursor = outcome.value.next;

      if (cursor === null) break;
    }

    set({ rows: collected, loading: false });
  },

  create: async (input) => {
    if (get().creating) return null;

    set({ creating: true });

    const outcome = await campaignCreate(input);

    set({ creating: false });

    if (!outcome.ok) {
      report(outcome.failure);

      return null;
    }

    await get().reload();

    return outcome.value;
  },

  start: async (campaignId) => {
    await control(campaignStart(campaignId), get().reload);
  },

  pause: async (campaignId) => {
    await control(campaignPause(campaignId), get().reload);
  },

  resume: async (campaignId) => {
    await control(campaignResume(campaignId), get().reload);
  },

  cancel: async (campaignId) => {
    await control(campaignCancel(campaignId), get().reload);
  },

  applyProgress: (reading) => {
    set((state) => ({
      progress: { ...state.progress, [reading.campaignId]: reading },
      rows: state.rows.map((row) =>
        row.campaignId === reading.campaignId
          ? {
              ...row,
              status: reading.status,
              sent: reading.accepted,
              failed: reading.failed,
              // `live` follows the reading and not the status: a campaign is
              // live until its FINAL event says otherwise, and that event is
              // the one the backend emits outside its paced loop. Deriving it
              // from the status instead would leave Pause and Reprendre offered
              // on a campaign that has completed.
              live: !reading.done,
            }
          : row,
      ),
    }));
  },
}));

/**
 * Runs one control, reports a refusal, and refreshes the list either way.
 *
 * "Either way" is the point: a start that was refused because its session is
 * not bound still leaves the row exactly as it was, and a reload is what proves
 * it on screen rather than leaving a button that looks broken.
 */
async function control(
  call: ReturnType<typeof campaignStart>,
  reload: () => Promise<void>,
): Promise<void> {
  const outcome = await call;

  if (!outcome.ok) {
    report(outcome.failure);
  }

  await reload();
}
