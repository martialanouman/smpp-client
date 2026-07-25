import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import "../i18n";
import { PlaceholderView } from "./PlaceholderView";

describe("PlaceholderView", () => {
  it("affiche le nom de l'application et le message d'attente traduits", () => {
    render(<PlaceholderView />);

    expect(screen.getByRole("heading", { name: "ShinobiSMPP" })).toBeInTheDocument();
    expect(screen.getByText(/jalon 001/)).toBeInTheDocument();
  });

  it("ne laisse aucune clé de traduction non résolue", () => {
    const { container } = render(<PlaceholderView />);

    // i18next renvoie la clé elle-même quand elle est absente du catalogue.
    // Cette assertion transforme un catalogue incomplet en test rouge plutôt
    // qu'en « app.placeholder » affiché à l'utilisateur.
    expect(container.textContent).not.toMatch(/app\./);
  });
});
