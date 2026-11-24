#![cfg(feature = "llama")]

use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use agent_core::inference::{
    ErrorCode, FinishReason, GenerationRequest, GenerationStats, InferenceError, ModelConfig,
};
use futures_executor::block_on;
use inference::{InferenceWorker, LlamaProvider, Phase};

#[test]
fn rejects_a_header_only_gguf_without_losing_worker() {
    let file = std::env::temp_dir().join(format!("agent-empty-model-{}.gguf", std::process::id()));
    let mut header = [0_u8; 24];
    header[..4].copy_from_slice(b"GGUF");
    header[4] = 3;
    std::fs::write(&file, header).unwrap();

    let worker = InferenceWorker::spawn(LlamaProvider::default).unwrap();
    let error = block_on(worker.load(file.clone(), ModelConfig::default()).unwrap())
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidModel);
    assert_eq!(worker.status().unwrap().phase, Phase::Empty);
    std::fs::remove_file(file).unwrap();
}

fn local_model() -> PathBuf {
    PathBuf::from(
        std::env::var_os("AGENT_TEST_MODEL").expect("set AGENT_TEST_MODEL to the local GGUF"),
    )
}

fn seeded(prompt: impl Into<String>) -> GenerationRequest {
    let mut request = GenerationRequest::new(prompt);
    request.seed = Some(42);
    request
}

fn stream(
    worker: &InferenceWorker,
    request: GenerationRequest,
) -> (inference::Pending<GenerationStats>, mpsc::Receiver<String>) {
    let (tx, rx) = mpsc::channel();
    let pending = worker
        .generate(request, move |delta| {
            tx.send(delta.text)
                .map_err(|_| InferenceError::new(ErrorCode::StreamClosed, "Test stream closed."))
        })
        .unwrap();
    (pending, rx)
}

fn generate(
    worker: &InferenceWorker,
    request: GenerationRequest,
) -> (GenerationStats, Vec<String>) {
    let (pending, chunks) = stream(worker, request);
    let stats = block_on(pending).unwrap().unwrap();
    let chunks: Vec<_> = chunks.into_iter().collect();
    assert!(!chunks.concat().trim().is_empty());
    assert!((1..=256).contains(&stats.generated_tokens));
    assert!(stats.time_to_first_token_ms.is_some());
    assert!(
        stats
            .prompt_tokens_per_second
            .is_some_and(|rate| rate > 0.0)
    );
    assert_ne!(stats.reason, FinishReason::Cancelled);
    (stats, chunks)
}

fn cancel_after_deltas(worker: &InferenceWorker, path: &std::path::Path) -> Duration {
    let (pending, rx) = stream(
        worker,
        seeded("Write a detailed story about a traveler who visits many distant cities."),
    );
    rx.recv_timeout(Duration::from_secs(30)).unwrap();
    rx.recv_timeout(Duration::from_secs(4)).unwrap();
    assert_eq!(worker.status().unwrap().phase, Phase::Generating);
    assert_eq!(
        worker
            .load(path.into(), ModelConfig::default())
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(
        worker
            .generate(seeded("conflict"), |_| Ok(()))
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(worker.unload().unwrap_err().code, ErrorCode::Busy);
    let started = Instant::now();
    assert!(worker.cancel().unwrap());
    assert_eq!(
        block_on(pending).unwrap().unwrap().reason,
        FinishReason::Cancelled
    );
    let latency = started.elapsed();
    assert!(
        latency < Duration::from_secs(2),
        "cancellation took {latency:?}"
    );
    assert_eq!(worker.status().unwrap().phase, Phase::Ready);
    latency
}

#[test]
#[ignore = "requires an explicitly supplied local GGUF via AGENT_TEST_MODEL"]
fn real_qwen_cpu_inference() {
    let path = local_model();
    let worker = InferenceWorker::spawn(LlamaProvider::default).unwrap();
    let model = block_on(worker.load(path.clone(), ModelConfig::default()).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(model.architecture, "qwen3");
    assert_eq!(model.config, ModelConfig::default());
    assert!(model.supports_thinking);
    println!(
        "model={}, bytes={}, parameters={}, load_ms={}",
        model.file_name, model.file_bytes, model.parameters, model.load_ms
    );

    let (stats, chunks) = generate(
        &worker,
        seeded("What is 2 plus 2? Answer with a short sentence."),
    );
    assert!(chunks.len() > 1);
    let response = chunks.concat();
    assert_eq!(stats.reason, FinishReason::EndOfGeneration);
    assert!(response.contains('4'));
    assert!(!response.contains("<think>"));
    println!(
        "direct: {stats:?}; chunks={}; response={response:?}",
        chunks.len()
    );

    let latency = cancel_after_deltas(&worker, &path);
    println!(
        "cancel_ms={:.3}; blocked generate, load and unload during the active request",
        latency.as_secs_f64() * 1000.0
    );

    let (stats, chunks) = generate(&worker, seeded("请只回答这四个汉字：你好世界"));
    let response = chunks.concat();
    assert!(response.contains("你好世界"), "{response:?}");
    assert!(!response.contains('\u{fffd}'));
    println!(
        "post-cancel UTF-8: {stats:?}; chunks={}; response={response:?}",
        chunks.len()
    );

    let mut thinking = seeded(
        "Solve this step by step: count all possible ordered pairs of integers between 1 and 100 whose sum is prime.",
    );
    thinking.enable_thinking = true;
    let (stats, chunks) = generate(&worker, thinking);
    let response = chunks.concat();
    assert!(
        response.contains("<think>"),
        "thinking response: {response:?}"
    );
    assert_eq!(stats.reason, FinishReason::TokenLimit);
    assert_eq!(stats.generated_tokens, 256);
    println!(
        "thinking: {stats:?}; chunks={}; prefix={:?}",
        chunks.len(),
        response.chars().take(100).collect::<String>()
    );

    block_on(worker.unload().unwrap()).unwrap().unwrap();
    assert_eq!(worker.status().unwrap().phase, Phase::Empty);
    assert_eq!(
        worker
            .generate(seeded("no model"), |_| Ok(()))
            .unwrap_err()
            .code,
        ErrorCode::NoModel
    );
    let reloaded = block_on(worker.load(path, ModelConfig::default()).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.parameters, model.parameters);
    let (stats, chunks) = generate(
        &worker,
        seeded("Reply with the name of the capital of France."),
    );
    assert!(chunks.concat().contains("Paris"));
    println!("after reload: {stats:?}; response={:?}", chunks.concat());
    block_on(worker.unload().unwrap()).unwrap().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires AGENT_TEST_MODEL and AGENT_BENCH_THREADS (2, 4 or 6)"]
fn benchmark_qwen_cpu() {
    let path = local_model();
    let threads = std::env::var("AGENT_BENCH_THREADS")
        .unwrap()
        .parse()
        .unwrap();
    assert!([2, 4, 6].contains(&threads));
    let hertz: f64 = std::env::var("AGENT_BENCH_CLK_TCK")
        .unwrap()
        .parse()
        .unwrap();
    let config = ModelConfig {
        threads,
        ..ModelConfig::default()
    };
    let worker = InferenceWorker::spawn(LlamaProvider::default).unwrap();
    let baseline = memory_kib("VmRSS");
    let model = block_on(worker.load(path.clone(), config).unwrap())
        .unwrap()
        .unwrap();

    let paragraph = "The observatory keeps records of clouds, winds, and clear skies. Each morning a technician cleans the lenses, reads the instruments, and writes the values in a notebook. The following story uses those records as its setting. ";
    let prompt = format!(
        "{}\nWrite a long story about an unexpected discovery at the observatory. Use simple language and at least 500 words.",
        paragraph.repeat(15)
    );
    for run in ["first", "warm1", "warm2"] {
        let before_cpu = cpu_ticks();
        let before_wall = Instant::now();
        let (stats, _) = generate(&worker, seeded(&prompt));
        let elapsed = before_wall.elapsed().as_secs_f64();
        let cpu_percent = 100.0 * (cpu_ticks() - before_cpu) as f64 / hertz / elapsed;
        assert_eq!(stats.reason, FinishReason::TokenLimit);
        println!(
            "threads={threads}, run={run}, prompt_tokens={}, output_tokens={}, ttft_ms={}, prompt_tps={:.2}, decode_tps={:.2}, cpu_pct={cpu_percent:.1}",
            stats.prompt_tokens,
            stats.generated_tokens,
            stats.time_to_first_token_ms.unwrap(),
            stats.prompt_tokens_per_second.unwrap(),
            stats.tokens_per_second
        );
    }

    let latency = cancel_after_deltas(&worker, &path);
    let loaded_rss = memory_kib("VmRSS");
    block_on(worker.unload().unwrap()).unwrap().unwrap();
    let after_unload = memory_kib("VmRSS");
    let peak = memory_kib("VmHWM");
    let reloaded = block_on(worker.load(path, config).unwrap())
        .unwrap()
        .unwrap();
    block_on(worker.unload().unwrap()).unwrap().unwrap();
    println!(
        "threads={threads}, load_ms={}, reload_ms={}, cancel_ms={:.3}, baseline_rss_kib={baseline}, loaded_rss_kib={loaded_rss}, peak_rss_kib={peak}, unloaded_rss_kib={after_unload}",
        model.load_ms,
        reloaded.load_ms,
        latency.as_secs_f64() * 1000.0,
    );
}

#[cfg(target_os = "linux")]
fn cpu_ticks() -> u64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    stat.rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .skip(11)
        .take(2)
        .map(|value| value.parse::<u64>().unwrap())
        .sum()
}

#[cfg(target_os = "linux")]
fn memory_kib(key: &str) -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name == key).then(|| value.split_whitespace().next().unwrap().parse().unwrap())
        })
        .unwrap()
}
