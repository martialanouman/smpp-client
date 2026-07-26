import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { AppShell } from "./components/AppShell";
import "./i18n";
import { startBackendBridge } from "./store/bridge";
import "./styles.css";

const rootElement = document.getElementById("root");

// `strict` surfaces this case instead of letting it slip under a `!`: an
// index.html without a mount point is a build bug, not a situation to recover
// from silently.
if (!rootElement) {
  throw new Error("Mount point #root not found in index.html");
}

// Started before the first render so preferences persisted by the backend are
// in place, rather than making the interface flash from the defaults to the
// stored values.
void startBackendBridge();

createRoot(rootElement).render(
  <StrictMode>
    <AppShell />
  </StrictMode>,
);
