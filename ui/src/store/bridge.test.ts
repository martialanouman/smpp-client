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

  it("still reads the preferences when the event subscription fails", async () => {
    // The Tauri API rejects outside a WebView. An unguarded await would skip
    // config_get entirely and leave the interface on its defaults, silently.
    onErrorNotify.mockRejectedValue(new Error("tauri api unavailable"));
    configGet.mockResolvedValue({ ok: true, value: CONFIG });
    const { startBackendBridge } = await import("./bridge");

    await startBackendBridge();

    expect(usePreferences.getState().language).toBe("en");
    expect(usePreferences.getState().notifications[0]?.code).toBeNull();
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

describe("persisting a preference", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    usePreferences.setState(usePreferences.getInitialState(), true);
    // A patch is merged onto the last confirmed configuration, so one must
    // exist. Starting from `null` is covered by its own test below.
    usePreferences.getState().adoptConfig(CONFIG);
  });

  it("resubmits the fields it does not change", async () => {
    configSet.mockResolvedValue({ ok: true, value: { ...CONFIG, theme: "light" } });
    const { persistPreference } = await import("./bridge");

    await persistPreference({ theme: "light" });

    // `config_set` replaces the whole configuration. Sending only `theme`
    // would reset logLevel and retentionDays to their defaults — a bug that
    // stays invisible until the next restart.
    expect(configSet).toHaveBeenCalledWith({
      language: CONFIG.language,
      theme: "light",
      logLevel: CONFIG.logLevel,
      retentionDays: CONFIG.retentionDays,
    });
  });

  it("refuses to write before anything has been read", async () => {
    usePreferences.setState(usePreferences.getInitialState(), true);
    const { persistPreference } = await import("./bridge");

    await persistPreference({ theme: "dark" });

    expect(configSet).not.toHaveBeenCalled();
    expect(usePreferences.getState().notifications).toHaveLength(1);
  });

  it("writes the change through config_set and adopts what the backend returns", async () => {
    configSet.mockResolvedValue({ ok: true, value: { ...CONFIG, language: "en", theme: "light" } });
    const { persistPreference } = await import("./bridge");

    await persistPreference({ language: "en" });

    expect(configSet).toHaveBeenCalledWith(expect.objectContaining({ language: "en" }));
    // The backend is the source of truth: the store adopts the config it
    // returns, not the value that was submitted. A backend that normalises or
    // rejects part of the input must win.
    expect(usePreferences.getState().language).toBe("en");
    expect(usePreferences.getState().theme).toBe("light");
  });

  it("does not notify twice when the backend already emitted the event", async () => {
    // `config_set` signals a failure twice by design: returned to the caller
    // and pushed on `error:notify`. Notifying on both produced two identical
    // toasts for one failure, each to be dismissed separately.
    //
    // Simulated here the way it happens for real: the event lands first, then
    // the call resolves with the same error.
    configSet.mockImplementation(() => {
      usePreferences.getState().notify({ code: "CONFIG_UNWRITABLE", message: "could not write" });

      return Promise.resolve({
        ok: false,
        failure: {
          kind: "backend",
          error: { code: "CONFIG_UNWRITABLE", message: "could not write", details: null },
        },
      });
    });
    const { persistPreference } = await import("./bridge");

    await persistPreference({ theme: "dark" });

    expect(usePreferences.getState().notifications).toHaveLength(1);
  });

  it("surfaces a rejected value and leaves the store untouched", async () => {
    configSet.mockResolvedValue({
      ok: false,
      failure: {
        kind: "backend",
        error: { code: "CONFIG_INVALID_LANGUAGE", message: "unsupported language", details: null },
      },
    });
    const { persistPreference } = await import("./bridge");

    await persistPreference({ language: "en" });

    // CA-001-05: the interface must not show a preference the backend refused
    // to store — that would survive on screen until the next restart and then
    // silently revert. The store keeps what `adoptConfig` last confirmed.
    expect(usePreferences.getState().language).toBe(CONFIG.language);
    // The notification comes from `error:notify`, not from here — see the test
    // above. What this one guards is that the store is left alone.
  });

  it("reports a transport failure without a fabricated code", async () => {
    configSet.mockResolvedValue({
      ok: false,
      failure: { kind: "transport", message: "bridge unavailable" },
    });
    const { persistPreference } = await import("./bridge");

    await persistPreference({ theme: "dark" });

    expect(usePreferences.getState().notifications[0]?.code).toBeNull();
    expect(usePreferences.getState().theme).toBe(CONFIG.theme);
  });
});
