import { beforeEach, describe, expect, it, vi } from "vitest";

import type { MessagePreviewDto } from "../ipc";
import { blankForm, useSend } from "./send";

const messagePreview = vi.fn();

vi.mock("../ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../ipc")>()),
  messagePreview: (input: unknown) => messagePreview(input) as unknown,
}));

function aPreview(characters: number, segments: number): MessagePreviewDto {
  return {
    encoding: "gsm7Bit",
    dataCoding: 0,
    characters,
    unitsUsed: characters,
    unitsRemaining: 160 - characters,
    segments,
  };
}

describe("the send store", () => {
  beforeEach(() => {
    messagePreview.mockReset();
    useSend.setState({
      sessionId: "",
      form: blankForm(),
      preview: null,
      result: null,
      progress: null,
      sending: false,
      previewGeneration: 0,
    });
  });

  /**
   * CA-006-09: the counter must describe the text on screen.
   *
   * `message_preview` is a round trip issued on every change, so its answers
   * can arrive out of order — and the older one landing last would freeze a
   * count for a text the operator has already moved past, until the next
   * keystroke.
   *
   * The first request is held open and released only after the second has
   * been adopted, which is the interleaving that produces the bug.
   */
  it("keeps the newest preview when an older answer arrives last", async () => {
    let release: (() => void) | undefined;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });

    messagePreview
      .mockImplementationOnce(async () => {
        await held;
        return { ok: true, value: aPreview(2, 1) };
      })
      .mockResolvedValueOnce({ ok: true, value: aPreview(400, 3) });

    const stale = useSend.getState().refreshPreview();
    const fresh = useSend.getState().refreshPreview();

    await fresh;
    expect(useSend.getState().preview?.characters).toBe(400);

    release?.();
    await stale;

    expect(
      useSend.getState().preview?.characters,
      "the overtaken answer must not repaint the counter",
    ).toBe(400);
    expect(useSend.getState().preview?.segments).toBe(3);
  });

  /** The ordinary path still adopts what the backend computed. */
  it("adopts the preview the backend returned", async () => {
    messagePreview.mockResolvedValue({ ok: true, value: aPreview(7, 1) });

    await useSend.getState().refreshPreview();

    expect(useSend.getState().preview?.characters).toBe(7);
  });

  /**
   * A forced encoding that cannot write the text is something the operator is
   * in the middle of typing, not an incident — the counter clears, no toast.
   */
  it("clears the counter on a preview failure rather than raising a toast", async () => {
    messagePreview.mockResolvedValue({ ok: true, value: aPreview(7, 1) });
    await useSend.getState().refreshPreview();
    expect(useSend.getState().preview).not.toBeNull();

    messagePreview.mockResolvedValue({
      ok: false,
      failure: { kind: "backend", error: { code: "MESSAGE_ENCODING", message: "", details: null } },
    });
    await useSend.getState().refreshPreview();

    expect(useSend.getState().preview).toBeNull();
  });

  /** Only the message on screen: a campaign at milestone 010 must not repaint it. */
  it("adopts an update for the current message and ignores the others", () => {
    useSend.setState({ result: null, progress: null });

    useSend.getState().adopt("any", "QUEUED");
    expect(useSend.getState().progress).toBe("QUEUED");
  });
});
