# v0.0.2 verification

## Reference model

The model was downloaded from Qwen's official Hugging Face repository. The
published file size and SHA-256 both matched the downloaded file. The model is
stored in the BACKUP volume under the project's model directory, outside Git.

| Field | Verified value |
| --- | --- |
| File | Qwen3-0.6B-Q4_K_M.gguf |
| Repository revision | 1eaf4d9657fe65ad10a51eab76a8db5b363bddaa |
| Model license | Apache-2.0 |
| File size | 396,704,416 bytes |
| SHA-256 | b0638f08417a2d3c8652760462eb5407c6e30173cf9608ad0820757a281eea0e |

[Publisher file metadata](https://huggingface.co/api/models/Qwen/Qwen3-0.6B-GGUF/tree/1eaf4d9657fe65ad10a51eab76a8db5b363bddaa)
[License at the pinned revision](https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/blob/1eaf4d9657fe65ad10a51eab76a8db5b363bddaa/LICENSE)

## Real inference checks

The Rust checks ran in a separate Linux network namespace with loopback down
and no default route. The pinned template produced the expected Qwen user and
assistant delimiters. In non-thinking mode it appended Qwen's empty thinking
block to the assistant prefix. In thinking mode it left the prefix open for
the model to generate reasoning.

The 4-thread lifecycle test streamed eight chunks yielding “2 plus 2 is 4.”
and returned the end-of-generation finish reason. It rejected a second
generation, load, and unload during an active stream, then acknowledged
cancellation in 30.9 ms. The next prompt produced the exact UTF-8 text
“你好世界” in two chunks without replacement characters. A thinking request
produced a reasoning prefix and stopped at 256 generated tokens. After unload
and reload, the model answered “The capital of France is Paris.” and reported
end of generation. A generation request while unloaded returned the structured
no-model error.

## Desktop verification

The Linux release ran in Xvfb in a separate network namespace with loopback
down. The test used the native picker to choose the verified model and loaded
it through the screen. While generation remained active, the visible response
grew from 1 to 14 characters and the Unload control stayed disabled. Clicking
Stop produced **Stopped** within 149 ms, including the accessibility probe's
polling delay. The next prompt completed with “Two plus two is four.” Enabling
thinking produced a stream beginning with the thinking marker. The test
unloaded the model, loaded the same selection again, and unloaded it.

## CPU benchmark

Recorded 2026-11-24 on Ubuntu 26.04.1, x86_64, Intel Core i5-8500, six physical
cores, and 14 GiB RAM. The same pinned model, 2048-token context, 698-token prompt
and 256-token output budget were used throughout. Sampling used temperature
0.7, top-p 0.8, top-k 20, seed 42, and non-thinking mode. Each run reached the
256-token limit. The processes ran sequentially at lower CPU and I/O priority
with networking disabled and zero GPU layers.

Each thread setting used a new process. “First” is the first generation after
model load. The next two generations reuse that loaded model and create fresh
contexts. The file cache was uncontrolled and had already been populated
during integrity checks, so these are process-first and warm results without
a cold-disk claim. Load includes native initialization, mapping, and template
validation. TTFT includes context creation and prompt evaluation. Prefill rate
covers prompt batch preparation and evaluation. Decode rate excludes the first
token. Process CPU percentage uses Linux user and system time during generation
and treats 100% as one fully occupied core.

| Threads | Run | TTFT (ms) | Prompt (tokens/s) | Decode (tokens/s) | Process CPU |
| ---: | --- | ---: | ---: | ---: | ---: |
| 2 | First | 4759 | 150.25 | 21.92 | 197.8% |
| 2 | Warm 1 | 4747 | 150.66 | 22.00 | 198.3% |
| 2 | Warm 2 | 4754 | 150.42 | 21.98 | 198.2% |
| 4 | First | 2504 | 291.93 | 35.38 | 391.1% |
| 4 | Warm 1 | 2507 | 291.63 | 34.82 | 389.8% |
| 4 | Warm 2 | 2509 | 291.35 | 35.31 | 390.5% |
| 6 | First | 2682 | 271.76 | 25.67 | 537.2% |
| 6 | Warm 1 | 2656 | 274.65 | 29.20 | 546.0% |
| 6 | Warm 2 | 2439 | 300.16 | 29.97 | 546.8% |

The cancellation probe ran after the three timed generations. Latency is the
time from requesting Stop on the worker to completion. Loaded RSS is the
process RSS after that probe, and unloaded RSS is the immediate measurement
after releasing the model. Peak RSS is the process high-water mark through
unload. The OS may retain cached file pages after their mapping is released.

| Threads | Load (ms) | Reload (ms) | Cancel (ms) | Baseline RSS (KiB) | Loaded RSS (KiB) | Peak RSS (KiB) | Unloaded RSS (KiB) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 2 | 414 | 371 | 33.265 | 6716 | 653760 | 886136 | 58280 |
| 4 | 399 | 374 | 28.591 | 6712 | 654656 | 886908 | 59176 |
| 6 | 407 | 360 | 30.017 | 6764 | 655696 | 888252 | 60216 |

Four threads delivered the highest decode throughput on this workload at
34.82–35.38 tokens/s. Six threads used more CPU and gave more variable rates.
The default remains four threads. Timing and memory depend on the model,
prompt, available CPU and memory, and other work on the machine.

## Repeating the checks

Compile once using the project's locked release profile. Set the file and
substitute the executable paths printed by Cargo for the inference library
unit tests and the qwen integration test.

```sh
cargo test --release --workspace --locked --no-run
export AGENT_TEST_MODEL=/path/to/Qwen3-0.6B-Q4_K_M.gguf
export AGENT_BENCH_CLK_TCK="$(getconf CLK_TCK)"
qwen=/path/to/compiled/qwen-test
inference_lib=/path/to/compiled/inference-lib-test

unshare --user --map-current-user --net "$inference_lib" \
  --ignored --exact llama::tests::renders_pinned_qwen_template_in_both_modes --nocapture
unshare --user --map-current-user --net "$qwen" \
  --ignored --exact real_qwen_cpu_inference --nocapture

for threads in 2 4 6; do
  AGENT_BENCH_THREADS="$threads" unshare --user --map-current-user --net "$qwen" \
    --ignored --exact benchmark_qwen_cpu --nocapture
done
```

## Known limits

Thinking tokens share the requested output limit. A long thinking segment can
reach that limit before a final answer, as observed in the 256-token test, and
the UI reports **Token limit reached**. Cancellation is cooperative between
native inference steps. This verification covers Linux CPU inference and the
pinned model. Its benchmark measurements are specific to this host and workload.
