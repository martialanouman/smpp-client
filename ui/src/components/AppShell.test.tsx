import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AppConfig, ConfigSetInput, IpcOutcome } from "../ipc";

import "../i18n";
import i18n from "../i18n";
import fr from "../i18n/locales/fr.json";
import en from "../i18n/locales/en.json";
import { SCREENS, usePreferences } from "../store/preferences";
import { AppShell } from "./AppShell";

const BASE: AppConfig = { language: "fr", theme: "system", logLevel: "info", retentionDays: 30 };

// The settings screen writes through `config_set` rather than mutating the
// store, because CA-001-02 requires a preference to survive a restart. Mocking
// the transport rather than `persistPreference` keeps the real path under
// test: merge, call, and adoption of what the backend confirms.
const configSet = vi.fn<(input: ConfigSetInput) => Promise<IpcOutcome<AppConfig>>>();

vi.mock("../ipc", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../ipc")>()),
  configSet: (input: ConfigSetInput) => configSet(input),
}));

describe("AppShell", () => {
  beforeEach(async () => {
    usePreferences.setState(usePreferences.getInitialState(), true);
    usePreferences.getState().adoptConfig(BASE);
    configSet.mockImplementation((input) =>
      Promise.resolve({
        ok: true,
        value: {
          ...BASE,
          language: input.language as AppConfig["language"],
          theme: input.theme as AppConfig["theme"],
        },
      }),
    );
    await i18n.changeLanguage("fr");
    document.documentElement.removeAttribute("data-theme");
  });

  it("opens on the dashboard", () => {
    render(<AppShell />);

    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(fr.nav.dashboard);
  });

  it("reaches all eight screens through the navigation", async () => {
    const user = userEvent.setup();
    render(<AppShell />);

    const nav = screen.getByRole("navigation", { name: fr.nav.label });

    for (const key of SCREENS) {
      await user.click(within(nav).getByRole("button", { name: fr.nav[key] }));

      // CA-001-01: the screen actually changes — asserting on the store alone
      // would pass even with a navigation wired to nothing.
      expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(fr.nav[key]);
      expect(usePreferences.getState().screen).toBe(key);
    }
  });

  it("marks the current screen for assistive technology", async () => {
    const user = userEvent.setup();
    render(<AppShell />);

    const nav = screen.getByRole("navigation", { name: fr.nav.label });
    const sessions = within(nav).getByRole("button", { name: fr.nav.sessions });

    expect(sessions).toHaveAttribute("aria-current", "false");
    await user.click(sessions);
    expect(sessions).toHaveAttribute("aria-current", "page");
  });

  it("switches to English without a reload", async () => {
    const user = userEvent.setup();
    render(<AppShell />);

    await user.click(screen.getByRole("button", { name: fr.nav.settings }));
    await user.selectOptions(screen.getByLabelText(fr.settings.language), "en");

    // CA-001-08: the visible text changes, not just the stored preference.
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent(en.nav.settings);
    expect(screen.getByRole("navigation", { name: en.nav.label })).toBeInTheDocument();
  });

  it("applies the theme to the document root", async () => {
    const user = userEvent.setup();
    render(<AppShell />);

    await user.click(screen.getByRole("button", { name: fr.nav.settings }));
    await user.selectOptions(screen.getByLabelText(fr.settings.theme), "dark");

    // The stylesheet keys off `data-theme`; setting state without reflecting it
    // on the DOM would leave the UI unchanged.
    expect(document.documentElement).toHaveAttribute("data-theme", "dark");

    await user.selectOptions(screen.getByLabelText(fr.settings.theme), "system");
    expect(document.documentElement).not.toHaveAttribute("data-theme");
  });

  it("shows a backend error as a dismissible toast", async () => {
    const user = userEvent.setup();
    render(<AppShell />);

    usePreferences
      .getState()
      .notify({ code: "CONFIG_INVALID_LANGUAGE", message: "langue inconnue" });

    const region = await screen.findByRole("region", { name: fr.notification.region });
    expect(within(region).getByText("langue inconnue")).toBeInTheDocument();
    // The stable code is what the interface can act on; showing it makes a bug
    // report actionable.
    expect(within(region).getByText(/CONFIG_INVALID_LANGUAGE/)).toBeInTheDocument();

    await user.click(within(region).getByRole("button", { name: fr.notification.dismiss }));
    expect(screen.queryByText("langue inconnue")).not.toBeInTheDocument();
  });

  it("renders no raw translation key", () => {
    render(<AppShell />);

    // i18next echoes a missing key back, so `nav.contacts` would render as-is.
    expect(document.body.textContent).not.toMatch(/\b(nav|screen|settings|empty)\.[a-zA-Z]/);
  });
});
