# v0.2.0 Task 06-A handoff (for Task 06-B)

All earlier Task 03 and Task 05 contracts still apply unless a change is listed here. The schema is now 10 and the upgrade is automatic. To downgrade, restore a pre-upgrade database backup.

## Chat with selected documents

```ts
type Grounding = "auto" | "documents";            // default "auto"
type ChatIntent = "general" | "search" | "question" | "overview";
knowledge?: { mode: RetrievalMode; documentIds: number[]; grounding?: Grounding } | null;

type KnowledgeCoverage = {
  /* Task 05 fields */
  intent?: ChatIntent;
  grounding?: Grounding;
  overview?: { documentId: number; suppliedChunks: number; totalChunks: number }[];
  requestedMode?: RetrievalMode;  // the dense mode that was asked for; `mode` is what ran
};
```

- **`auto`** makes the selection available without blocking Chat. When no passage qualifies, the host answers with `intent: "general"`. The coverage event is still sent, but the attempt is ordinary: it has no `Message.context` and its answer is reused in later prompts. Show it as an answer without document evidence. A dense model or index that is unavailable, missing or incompatible does not reject under `auto`. The host ranks by keyword, `mode` is `keyword` and `requestedMode` is the dense mode. Readiness should only advise in this case. `knowledgeReadiness` already does this.
- **`documents`** keeps the Task 05 behaviour: dense errors reject preparation, and an empty result gives the "cannot answer" response.
- **`search`** lists exact-phrase hits in document order. With `retrieved: 0`, show "not found".
- **`overview`** samples up to 4 passages. Show the counts truthfully: supplied of total.
- Citations, the evidence resolver, fitted counts, cancellation and continuation rules are unchanged for grounded intents.

## Models

| Command | Arguments | Result / behaviour |
| --- | --- | --- |
| `list_models` | — | `ModelReference[]` with copies of the same weights collapsed. Refresh it after load, import and reset. |
| `reset_model_settings` | `{ modelId }` | Updated `ModelReference` with `automaticSettings: true`. For the loaded model, sampling applies now. Context and threads apply at the next load. |
| `load_model` | `{ modelId, settings }` | Pass the saved settings to mean "this model's settings": automatic models get the current recommendation. Different settings become custom. |
| `save_model_settings` | `{ modelId, settings }` | Changed settings become custom. |
| `choose_model` | — | May return an existing row (the same weights, including the bundled model). The chosen file is never modified. |
| `unload_model` | — | Remembered. `prepare_bundled_model().autoLoad` stays false until a model is loaded again. |

`ModelReference` adds `bundled` (show "Built-in" only when true) and `automaticSettings`. `referenceProfile(reference)` in `models.ts` gives the correct label.

## Embedding model

`knowledge_embedding_status().restore` is `not_remembered`, `pending`, `restored`, `unloaded`, `missing`, `incompatible` or `failed`.
- The host restores the remembered BGE file at launch after full checksum verification. `useKnowledge` polls while the state is `pending`.
- `missing` keeps the choice. Suggest reconnecting the drive or choosing the file again.
- `incompatible` means the file was replaced, and the choice was forgotten.
- Choosing a file remembers it. Unloading keeps the path but disables restore.

No path ever reaches the frontend.

## Left for 06-B

- Categorized Settings, with automatic and custom settings presented clearly.
- A clearer UI for routed intents and partial overviews.
- An interactive pass over the Models, Knowledge and Chat paths.

Current UI additions are deliberately minimal: an "Answer only from documents" switch, a "Use automatic settings" link and coverage lines. Limitations are listed in `task-v0.2.0-06-a-report.md`.
