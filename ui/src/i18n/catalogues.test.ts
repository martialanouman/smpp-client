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

  /**
   * **Every literal key a component asks for exists.**
   *
   * The two tests above check the catalogues against *each other*; neither
   * checks them against the components. i18next does not throw on a missing
   * key — it renders the key itself — so `sessions.field.gsm7Charset` appeared
   * verbatim as a form label, in both languages, with a green suite. It was
   * found by launching the application and reading the screen.
   *
   * This scans the sources for `t("…")` and t(`…`) with no interpolation and
   * asserts each one resolves. Interpolated keys — t(`nav.${screen}`) — cannot
   * be checked this way, and are covered by the first test, which enumerates
   * their bases.
   */
  it("resolves every literal key the components ask for", async () => {
    const { readdirSync, readFileSync, statSync } = await import("node:fs");
    const { dirname, join } = await import("node:path");
    const { fileURLToPath } = await import("node:url");

    const sources = (directory: string): string[] =>
      readdirSync(directory).flatMap((entry) => {
        const path = join(directory, entry);

        if (statSync(path).isDirectory()) {
          return sources(path);
        }

        return /\.tsx?$/.test(entry) && !/\.test\.tsx?$/.test(entry) ? [path] : [];
      });

    const resolves = (key: string): boolean =>
      key
        .split(".")
        .reduce<unknown>(
          (node, part) =>
            typeof node === "object" && node !== null && part in node
              ? (node as Record<string, unknown>)[part]
              : undefined,
          fr,
        ) !== undefined;

    const missing: string[] = [];
    const root = join(dirname(fileURLToPath(import.meta.url)), "..");

    for (const path of sources(root)) {
      const source = readFileSync(path, "utf8");

      for (const match of source.matchAll(/\bt\(\s*["`]([a-zA-Z0-9_.]+)["`]/gu)) {
        const key = match[1];

        if (key !== undefined && !resolves(key)) {
          missing.push(`${key} (${path.split("/src/")[1] ?? path})`);
        }
      }
    }

    expect(missing, "keys asked for by a component and absent from fr.json").toEqual([]);
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
