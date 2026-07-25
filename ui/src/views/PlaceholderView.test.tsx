import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import "../i18n";
import fr from "../i18n/locales/fr.json";
import { PlaceholderView } from "./PlaceholderView";

describe("PlaceholderView", () => {
  it("renders the translated application name and holding message", () => {
    render(<PlaceholderView />);

    // Asserting against the catalogue rather than a literal keeps this test
    // independent of the wording: rephrasing the message is not a regression.
    expect(screen.getByRole("heading", { name: fr.app.name })).toBeInTheDocument();
    expect(screen.getByText(fr.app.placeholder)).toBeInTheDocument();
  });

  it("leaves no translation key unresolved", () => {
    const { container } = render(<PlaceholderView />);

    // i18next echoes the key back when it is missing from the catalogue.
    // This assertion turns an incomplete catalogue into a failing test rather
    // than an "app.placeholder" shown to the user.
    expect(container.textContent).not.toMatch(/app\./);
  });
});
