import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./i18n";
import "./styles.css";
import { PlaceholderView } from "./views/PlaceholderView";

const rootElement = document.getElementById("root");

// `strict` surfaces this case instead of letting it slip under a `!`: an
// index.html without a mount point is a build bug, not a situation to recover
// from silently.
if (!rootElement) {
  throw new Error("Mount point #root not found in index.html");
}

createRoot(rootElement).render(
  <StrictMode>
    <PlaceholderView />
  </StrictMode>,
);
