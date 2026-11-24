#![cfg(feature = "llama")]

use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use agent_core::inference::{
    ErrorCode, FinishReason, GenerationRequest, InferenceError, ModelConfig,
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

#[test]
#[ignore = "requires an explicitly supplied local GGUF via AGENT_TEST_MODEL"]
fn real_qwen_cpu_inference() {
    let path = PathBuf::from(
        std::env::var_os("AGENT_TEST_MODEL").expect("set AGENT_TEST_MODEL to the local GGUF"),
    );
    let worker = InferenceWorker::spawn(LlamaProvider::default).unwrap();
    let model = block_on(worker.load(path, ModelConfig::default()).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(model.architecture, "qwen3");
    assert_eq!(model.config, ModelConfig::default());
    println!(
        "model={}, bytes={}, parameters={}, load_ms={}",
        model.file_name, model.file_bytes, model.parameters, model.load_ms
    );

    let (tx, rx) = mpsc::channel();
    let mut request = GenerationRequest::new("What is 2 plus 2? Answer with a short sentence.");
    request.seed = Some(42);
    let done = worker
        .generate(request, move |delta| {
            tx.send(delta.text)
                .map_err(|_| InferenceError::new(ErrorCode::StreamClosed, "Test stream closed."))
        })
        .unwrap();
    let stats = block_on(done).unwrap().unwrap();
    let output: String = rx.into_iter().collect();
    assert!(!output.trim().is_empty());
    assert!(stats.generated_tokens > 0 && stats.generated_tokens <= 256);
    assert!(stats.time_to_first_token_ms.is_some());
    assert_ne!(stats.reason, FinishReason::Cancelled);
    println!("{stats:?}\nresponse={output:?}");

    let (tx, rx) = mpsc::channel();
    let mut request = GenerationRequest::new(
        "Write a detailed story about a traveler who visits many distant cities.",
    );
    request.max_tokens = 1024;
    let pending = worker
        .generate(request, move |delta| {
            tx.send(delta.text)
                .map_err(|_| InferenceError::new(ErrorCode::StreamClosed, "Test stream closed."))
        })
        .unwrap();
    rx.recv_timeout(Duration::from_secs(30)).unwrap();
    let stop = Instant::now();
    assert!(worker.cancel().unwrap());
    let cancelled = block_on(pending).unwrap().unwrap();
    assert_eq!(cancelled.reason, FinishReason::Cancelled);
    let latency = stop.elapsed();
    assert!(
        latency < Duration::from_secs(2),
        "cancellation took {latency:?}"
    );
    println!("cancellation_ms={:.3}", latency.as_secs_f64() * 1000.0);

    block_on(worker.unload().unwrap()).unwrap().unwrap();
    assert_eq!(worker.status().unwrap().phase, Phase::Empty);
    #[cfg(target_os = "linux")]
    if let Ok(status) = std::fs::read_to_string("/proc/self/status")
        && let Some(peak) = status.lines().find(|line| line.starts_with("VmHWM:"))
    {
        println!(
            "process_peak_rss={}",
            peak.trim_start_matches("VmHWM:").trim()
        );
    }
}
