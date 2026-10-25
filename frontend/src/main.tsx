import React from "react";
import ReactDOM from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import App from "./App";
import { OverlayProvider } from "./components/ui/Overlays";
import "./index.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <BrowserRouter>
      <OverlayProvider>
        <App />
      </OverlayProvider>
    </BrowserRouter>
  </React.StrictMode>,
);

