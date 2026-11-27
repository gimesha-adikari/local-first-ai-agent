// Development-only local preview. No clipboard, model, credential or network access.
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { Channel } from "@tauri-apps/api/core";
import type { GlanceInput, GlanceReceipt, GlanceState } from "../glance";

export function installGlanceFixtures() {
  const state: GlanceState = {
    localAvailable: true, localOnly: false, memoryEnabled: true,
    providers: [{ id: 1, name: "Example provider", model: "sample-model", locality: "cloud", revision: "fixture",
      sharing: { memories: true, documents: true }, retention: "none" }],
    documents: [{ id: 1, name: "Orchard notes" }],
  };
  let stopped = false;
  mockWindows("glance");
  mockIPC(async (name, args) => {
    switch (name) {
      case "glance_state": return state;
      case "glance_clipboard": return "The orchard uses two tanks to collect rainwater for dry weeks.";
      case "glance_run": {
        const { input, deltas } = args as { input: GlanceInput; deltas: Channel<string> };
        if (input.provider && !input.share) throw new Error("Sharing confirmation required.");
        const receipt: GlanceReceipt = {
          state: "completed", capability: "language.generate", provider: { id: input.provider ? "sample-model" : "local-model", locality: input.provider ? "third_party" : "on_device", retention: "none" },
          used: [{ kind: "prompt" }], withheld: [], application_retention: "session",
          external: input.provider ? [{ origin: "example.invalid", transmission: "sent", labels: ["user.input"] }] : [],
        };
        stopped = false;
        let text = "";
        for (const word of ["The orchard ", "stores rainwater ", "for irrigation."]) {
          await new Promise(resolve => setTimeout(resolve, 100));
          if (stopped) { receipt.state = "cancelled"; break; }
          deltas.onmessage(word);
          text += word;
        }
        return { text, receipt, error: stopped ? "Stopped." : null };
      }
      case "glance_stop": stopped = true; return;
      case "glance_copy": case "glance_remember": return;
      case "glance_dismiss": location.reload(); return;
      case "glance_open_main": location.assign(location.pathname); return;
      default: throw new Error("Unknown Glance command.");
    }
  });
}
