import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles/tokens.css";
import "./styles/app.css";

async function start() {
  // Outside Tauri (plain `vite dev`, end-to-end tests) use the in-browser mock backend.
  if (!("__TAURI_INTERNALS__" in window) || import.meta.env.VITE_MOCK === "1") {
    const mock = await import("./dev/mock");
    mock.installMock();
    if (new URLSearchParams(location.search).has("sample"))
      setTimeout(() => mock.openMockSample(), 300);
  }
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

start();
