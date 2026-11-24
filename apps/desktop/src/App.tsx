import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";

export default function App() {
  const [message, setMessage] = useState("Run the check to verify the connection.");
  const [pending, setPending] = useState(false);

  async function checkCore() {
    setPending(true);
    setMessage("Checking the Rust core…");

    try {
      setMessage(await invoke<string>("core_status"));
    } catch (error) {
      console.error("The Rust core check failed.", error);
      setMessage("Could not reach the Rust core. Try the check again.");
    } finally {
      setPending(false);
    }
  }

  return (
    <main className="grid min-h-screen place-items-center bg-slate-950 px-6 py-12 text-slate-100">
      <div className="w-full max-w-xl">
        <h1 className="text-3xl font-semibold tracking-tight">
          local-first-ai-agent
        </h1>
        <section
          aria-labelledby="core-status-heading"
          className="mt-8 rounded-xl border border-slate-700 bg-slate-900 p-6"
        >
          <h2 id="core-status-heading" className="text-lg font-medium">
            Rust core status
          </h2>
          <p role="status" className="mt-2 text-sm leading-6 text-slate-300">
            {message}
          </p>
          <button
            type="button"
            onClick={checkCore}
            disabled={pending}
            className="mt-5 rounded-md bg-teal-300 px-4 py-2 text-sm font-semibold text-slate-950 hover:bg-teal-200 focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-teal-300 disabled:cursor-wait disabled:opacity-60"
          >
            Check core connection
          </button>
        </section>
      </div>
    </main>
  );
}
