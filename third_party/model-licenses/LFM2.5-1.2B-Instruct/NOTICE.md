# LFM2.5-1.2B-Instruct model notice

Linux packages up to v0.2.0 included the LFM2.5-1.2B-Instruct language
model by Liquid AI, Inc. as a separate resource file. Since stage 03-C the
package ships no model; the same file can be imported as a checksum-verified
local model (curated manifest entry `lfm2.5-1.2b-instruct-q4_k_m`). The model is licensed under the LFM Open License v1.0
(`LICENSE` in this directory, installed next to the model). That license
applies only to the model file. It does not license the application, and
the application's own license does not cover the model.

| Field | Value |
| --- | --- |
| Model | LFM2.5-1.2B-Instruct, GGUF, Q4_K_M quantization (`general.file_type` 15) |
| File | `LFM2.5-1.2B-Instruct-Q4_K_M.gguf`, 730,895,168 bytes |
| SHA-256 | `b1b3de114215d9507409a662a501a631095a479a419584e8a2ded6304b19b4f5` |
| Source | Hugging Face `LiquidAI/LFM2.5-1.2B-Instruct-GGUF`, revision `8ed288026e23958ad9dfa92d53ed773a8eee7125` |
| Base model | `LiquidAI/LFM2.5-1.2B-Instruct` |
| License | LFM Open License v1.0 (`lfm1.0`), license file SHA-256 `5188f2b355da20647257a3156db5834c794e5fb5e6d8dc4d4cdbb3180e75b85b` |
| Modifications | None. The file is distributed byte-for-byte as published. |

Copyright Liquid AI, Inc. "LFM" and "Liquid AI" are used only to identify
the origin of the model. The upstream repository did not publish a NOTICE
file at the recorded revision.

## Commercial-use restriction

Section 5 of the LFM Open License limits commercial use. Commercial use
is licensed only while the using entity (including its affiliates) stays
below US$10 million in annual revenue. An entity at or above that
threshold needs a separate agreement with Liquid AI for commercial use.
Qualified non-profit organizations are exempt from the threshold for
non-commercial or research purposes. Anyone redistributing the model must
pass on a copy of the license. Read the full `LICENSE` for the binding
terms; this summary is not legal advice.

Packagers who cannot meet these terms should build without the bundled
model (the default `tauri build`) and let users import a model themselves.
