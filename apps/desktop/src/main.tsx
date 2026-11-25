import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error("The application root is missing.");
}

async function start(container: HTMLElement) {
  // Development-only synthetic IPC for inspecting the UI in an ordinary browser.
  // `import.meta.env.DEV` is false in production builds, so Vite removes this branch.
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    const { installFixtures } = await import("./dev/fixtures");
    await installFixtures(new URLSearchParams(window.location.search).get("fixture") ?? "default");
  }
  createRoot(container).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

void start(root);
