import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error("The application root is missing.");
}

async function start(container: HTMLElement) {
  const query = new URLSearchParams(window.location.search);
  const glance = query.get("glance") === "1";
  // Development-only synthetic IPC for inspecting the UI in an ordinary browser.
  // `import.meta.env.DEV` is false in production builds, so Vite removes this branch.
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    if (glance) {
      const { installGlanceFixtures } = await import("./dev/glanceFixtures");
      installGlanceFixtures();
    } else {
      const { installFixtures } = await import("./dev/fixtures");
      await installFixtures(query.get("fixture") ?? "default");
    }
  }
  const { default: Surface } = glance ? await import("./Glance") : await import("./App");
  createRoot(container).render(
    <StrictMode>
      <Surface />
    </StrictMode>,
  );
}

void start(root);
