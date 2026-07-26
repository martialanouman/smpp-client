/**
 * Light integration test of the boundary: `config_set` then `config_get`
 * through the **generated** wrappers (milestone 001 §5).
 *
 * `mockIPC` intercepts at the same place Tauri does, so the command names, the
 * argument envelope and the `Result` shape are all exercised for real; only the
 * Rust side is stubbed. What this test cannot see — that the backend actually
 * validates — is covered by the Rust unit tests.
 */

import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";

import { configGet, configSet } from "./index";
import type { AppConfig, ConfigSetInput, ErrorDto } from "./index";

const DEFAULTS: AppConfig = {
  language: "fr",
  theme: "system",
  logLevel: "info",
  retentionDays: 30,
};

/** Stands in for the Rust store: keeps whatever `config_set` accepted. */
function mockBackend(reject?: ErrorDto) {
  let stored: AppConfig = DEFAULTS;

  mockIPC((command, payload) => {
    if (command === "config_get") {
      return stored;
    }

    if (command === "config_set") {
      if (reject) {
        throw reject;
      }

      const { input } = payload as { input: ConfigSetInput };
      stored = {
        language: input.language as AppConfig["language"],
        theme: input.theme as AppConfig["theme"],
        logLevel: input.logLevel as AppConfig["logLevel"],
        retentionDays: input.retentionDays,
      };
      return stored;
    }

    throw new Error(`unexpected command: ${command}`);
  });
}

afterEach(clearMocks);

describe("ipc boundary", () => {
  it("reads back through config_get what config_set accepted", async () => {
    mockBackend();

    const written = await configSet({
      language: "en",
      theme: "dark",
      logLevel: "debug",
      retentionDays: 90,
    });

    expect(written).toEqual({
      ok: true,
      value: { language: "en", theme: "dark", logLevel: "debug", retentionDays: 90 },
    });
    expect(await configGet()).toEqual(written);
  });

  it("surfaces a rejected input as a backend failure carrying the stable code", async () => {
    const error: ErrorDto = {
      code: "CONFIG_INVALID_LANGUAGE",
      message: "unsupported language",
      details: { field: "language", allowed: "fr, en" },
    };
    mockBackend(error);

    const outcome = await configSet({
      language: "kl",
      theme: "dark",
      logLevel: "info",
      retentionDays: 30,
    });

    expect(outcome).toEqual({ ok: false, failure: { kind: "backend", error } });
  });

  it("distinguishes a bridge failure from a backend rejection", async () => {
    mockIPC(() => {
      throw new Error("the IPC bridge is unavailable");
    });

    const outcome = await configGet();

    expect(outcome).toEqual({
      ok: false,
      failure: { kind: "transport", message: "the IPC bridge is unavailable" },
    });
  });

  it("never throws, whatever the backend does", async () => {
    mockIPC(() => {
      throw "a bare string, not an Error";
    });

    await expect(configGet()).resolves.toMatchObject({ ok: false });
  });
});

describe("a rejection that is not a DTO", () => {
  it("is reported as transport, not as a backend error", async () => {
    // Tauri rejects argument deserialisation with a bare JSON STRING, not an
    // object: `retentionDays: -1` through a hand-made invoke yields
    // "invalid args `input` for command `config_set`: …".
    //
    // Labelling that `backend` meant reading `.code` off a string — undefined —
    // and rendering a toast with neither code nor message. Exactly the class of
    // input CA-001-05 is about.
    mockIPC(() => {
      throw "invalid args `input` for command `config_set`: invalid value: integer `-1`";
    });

    const outcome = await configGet();

    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.failure.kind).toBe("transport");
    if (outcome.failure.kind !== "transport") return;
    expect(outcome.failure.message).toContain("invalid args");
  });

  it("is reported as transport when the object carries no code", async () => {
    mockIPC(() => {
      throw { message: "no code here" };
    });

    const outcome = await configGet();

    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.failure.kind).toBe("transport");
  });

  it("still recognises a well-formed DTO as a backend error", async () => {
    // The contrast matters: a narrower that classed everything as transport
    // would pass the two tests above while destroying the distinction.
    mockIPC(() => {
      throw { code: "CONFIG_UNWRITABLE", message: "could not write", details: null };
    });

    const outcome = await configGet();

    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.failure.kind).toBe("backend");
    if (outcome.failure.kind !== "backend") return;
    expect(outcome.failure.error.code).toBe("CONFIG_UNWRITABLE");
  });
});
