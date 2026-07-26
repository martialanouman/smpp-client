import { beforeEach, describe, expect, it, vi } from "vitest";

import { SCREENS, usePreferences } from "./preferences";

/**
 * The store is the single source of truth for language, theme and the active
 * screen. It is tested without React on purpose: a Zustand store is plain
 * state, and rendering a component to check a reducer only makes failures
 * harder to read.
 */
describe("preferences store", () => {
  beforeEach(() => {
    usePreferences.setState(usePreferences.getInitialState(), true);
  });

  it("starts on the dashboard, in French, following the system theme", () => {
    const state = usePreferences.getState();

    expect(state.screen).toBe("dashboard");
    expect(state.language).toBe("fr");
    expect(state.theme).toBe("system");
  });

  it("navigates to every one of the eight screens", () => {
    expect(SCREENS).toHaveLength(8);

    for (const screen of SCREENS) {
      usePreferences.getState().goTo(screen);
      expect(usePreferences.getState().screen).toBe(screen);
    }
  });

  it("switches language and theme independently", () => {
    usePreferences.getState().setLanguage("en");
    expect(usePreferences.getState().language).toBe("en");
    expect(usePreferences.getState().theme).toBe("system");

    usePreferences.getState().setTheme("dark");
    expect(usePreferences.getState().theme).toBe("dark");
    expect(usePreferences.getState().language).toBe("en");
  });

  describe("notifications", () => {
    it("keeps them in arrival order and drops them one by one", () => {
      const { notify } = usePreferences.getState();

      notify({ code: "CONFIG_INVALID_LANGUAGE", message: "first" });
      notify({ code: "CONFIG_UNWRITABLE", message: "second" });

      const [first, second] = usePreferences.getState().notifications;
      expect(first?.message).toBe("first");
      expect(second?.message).toBe("second");

      const firstId = first?.id;
      if (firstId === undefined) throw new Error("no notification to dismiss");

      usePreferences.getState().dismiss(firstId);
      expect(usePreferences.getState().notifications).toHaveLength(1);
      expect(usePreferences.getState().notifications[0]?.message).toBe("second");
    });

    it("bounds the queue so a backend loop cannot grow it without end", () => {
      const { notify } = usePreferences.getState();

      for (let index = 0; index < 50; index += 1) {
        notify({ code: "CONFIG_UNWRITABLE", message: `error ${index}` });
      }

      const { notifications } = usePreferences.getState();
      expect(notifications.length).toBeLessThanOrEqual(5);
      // The most recent must survive: dropping the newest would hide the error
      // the user just caused.
      expect(notifications.at(-1)?.message).toBe("error 49");
    });

    it("gives each notification a distinct id even within the same millisecond", () => {
      vi.spyOn(Date, "now").mockReturnValue(1_000);
      const { notify } = usePreferences.getState();

      notify({ code: "CONFIG_UNWRITABLE", message: "a" });
      notify({ code: "CONFIG_UNWRITABLE", message: "b" });

      const ids = usePreferences.getState().notifications.map((entry) => entry.id);
      expect(new Set(ids).size).toBe(ids.length);

      vi.restoreAllMocks();
    });
  });
});
