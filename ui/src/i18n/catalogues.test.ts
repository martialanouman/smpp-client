import { describe, expect, it } from "vitest";

import { SCREENS } from "../store/preferences";
import en from "./locales/en.json";
import fr from "./locales/fr.json";

/**
 * Guards the catalogues rather than the rendering.
 *
 * CLAUDE.md §4 forbids hard-coded strings in components, which only holds if
 * every key a component asks for actually exists. A missing key does not throw
 * in i18next — it renders the key itself, so `nav.contacts` ends up on screen.
 * These tests turn that into a red build.
 */
describe("translation catalogues", () => {
  it("names and describes each of the eight screens, in both languages", () => {
    for (const screen of SCREENS) {
      expect(fr.nav, `fr is missing nav.${screen}`).toHaveProperty(screen);
      expect(en.nav, `en is missing nav.${screen}`).toHaveProperty(screen);
    }
  });

  it("has exactly the same keys on both sides", () => {
    const flatten = (value: unknown, prefix = ""): string[] =>
      typeof value === "object" && value !== null
        ? Object.entries(value).flatMap(([key, nested]) =>
            flatten(nested, prefix ? `${prefix}.${key}` : key),
          )
        : [prefix];

    // A key present in French and absent in English falls back silently to
    // French, which reads as a rendering bug rather than a missing translation.
    expect(flatten(en).sort()).toEqual(flatten(fr).sort());
  });

  it("leaves no value empty", () => {
    const values = (value: unknown): string[] =>
      typeof value === "object" && value !== null
        ? Object.values(value).flatMap(values)
        : [String(value)];

    for (const text of values(fr).concat(values(en))) {
      expect(text.trim()).not.toBe("");
    }
  });
});
