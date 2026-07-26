import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AppConfig, ErrorNotify, IpcOutcome } from "../ipc";
import { usePreferences } from "./preferences";

const configGet = vi.fn<() => Promise<IpcOutcome<AppConfig>>>();
const configSet = vi.fn<(input: unknown) => Promise<IpcOutcome<AppConfig>>>();
const onErrorNotify = vi.fn<(handler: (payload: ErrorNotify) => void) => Promise<() => void>>();

vi.mock("../ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../ipc")>()),
  configGet: () => configGet(),
  configSet: (input: unknown) => configSet(input),
  onErrorNotify: (handler: (payload: ErrorNotify) => void) => onErrorNotify(handler),
}));

const CONFIG: AppConfig = {
  language: "en",
  theme: "dark",
  logLevel: "debug",
  retentionDays: 30,
};

describe("backend bridge", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    usePreferences.setState(usePreferences.getInitialState(), true);
    onErrorNotify.mockResolvedValue(() => undefined);
  });

  it("adopts the preferences the backend has persisted", async () => {
    configGet.mockResolvedValue({ ok: true, value: CONFIG });
    const { startBackendBridge } = await import("./bridge");

    await startBackendBridge();

    expect(usePreferences.getState().language).toBe("en");
    expect(usePreferences.getState().theme).toBe("dark");
  });

  it("keeps the defaults and reports when the backend cannot be read", async () => {
    configGet.mockResolvedValue({
      ok: false,
      failure: { kind: "transport", message: "bridge unavailable" },
    });
    const { startBackendBridge } = await import("./bridge");

    await startBackendBridge();

    // Falling back to the defaults beats rendering nothing: the interface must
    // stay usable when only persistence is broken.
    expect(usePreferences.getState().language).toBe("fr");
    expect(usePreferences.getState().notifications).toHaveLength(1);
  });

  it("turns an error:notify event into a notification", async () => {
    configGet.mockResolvedValue({ ok: true, value: CONFIG });
    const { startBackendBridge } = await import("./bridge");

    await startBackendBridge();

    const handler = onErrorNotify.mock.calls[0]?.[0];
    if (!handler) throw new Error("the bridge did not subscribe to error:notify");

    handler({ code: "CONFIG_UNWRITABLE", message: "disk full" });

    expect(usePreferences.getState().notifications[0]?.message).toBe("disk full");
  });
});
