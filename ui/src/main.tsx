import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./i18n";
import "./styles.css";
import { PlaceholderView } from "./views/PlaceholderView";

const racine = document.getElementById("root");

// `strict` rend ce cas visible plutôt que de le laisser passer sous un `!` :
// un index.html sans point de montage est un bug de build, pas une situation
// à rattraper silencieusement.
if (!racine) {
  throw new Error("Point de montage #root introuvable dans index.html");
}

createRoot(racine).render(
  <StrictMode>
    <PlaceholderView />
  </StrictMode>,
);
