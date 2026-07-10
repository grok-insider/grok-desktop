import "@fontsource-variable/ibm-plex-sans/index.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "@fontsource/ibm-plex-mono/600.css";
import React from "react";
import ReactDOM from "react-dom/client";
import { HashRouter } from "react-router-dom";
import { App } from "./app/App";
import { DesktopBridgeUnavailable } from "./app/DesktopBridgeUnavailable";
import { DesktopClientProvider } from "./services/DesktopClientContext";
import { createRendererDesktopClient } from "./services/rendererDesktopClient";
import "./styles.css";

const selection = createRendererDesktopClient(
  window.grokDesktop,
  import.meta.env.VITE_BROWSER_PREVIEW === "true",
);

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {selection.kind === "ready" ? (
      <DesktopClientProvider client={selection.client}>
        <HashRouter>
          <App />
        </HashRouter>
      </DesktopClientProvider>
    ) : <DesktopBridgeUnavailable />}
  </React.StrictMode>,
);
