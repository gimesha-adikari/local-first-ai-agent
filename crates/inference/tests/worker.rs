use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use agent_core::inference::{
    Cancellation, ErrorCode, FinishReason, GenerationRequest, GenerationStats, InferenceError,
    InferenceProvider, ModelConfig, ModelInfo, Result, TextDelta,
};
use futures_executor::block_on;
use inference::{InferenceWorker, Phase};

struct Mock {
    releases: Arc<AtomicUsize>,
    loaded: bool,
}

impl InferenceProvider for Mock {
    fn load(&mut self, path: &Path, config: ModelConfig) -> Result<ModelInfo> {
        if path == Path::new("rejected.gguf") {
            return Err(InferenceError::new(
                ErrorCode::InvalidModel,
                "Rejected fixture.",
            ));
        }
        self.loaded = true;
        Ok(ModelInfo {
            file_name: "mock.gguf".into(),
            name: "Mock".into(),
            architecture: "mock".into(),
            parameters: 1,
            file_bytes: 32,
            trained_context: 2048,
            supports_thinking: false,
            config,
            load_ms: 0,
        })
    }

    fn unload(&mut self) {
        if std::mem::take(&mut self.loaded) {
            self.releases.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn generate(
        &mut self,
        request: &GenerationRequest,
        cancel: &Cancellation,
        emit: &mut dyn FnMut(TextDelta) -> Result<()>,
    ) -> Result<GenerationStats> {
        assert!(self.loaded);
        emit(TextDelta {
            text: "first".into(),
        })?;
        if request.prompt == "wait for stop" {
            let until = std::time::Instant::now() + Duration::from_secs(2);
            while !cancel.is_cancelled() && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(cancel.is_cancelled());
        } else {
            emit(TextDelta {
                text: " second".into(),
            })?;
        }
        Ok(GenerationStats {
            reason: if cancel.is_cancelled() {
                FinishReason::Cancelled
            } else {
                FinishReason::EndOfGeneration
            },
            prompt_tokens: 2,
            prompt_tokens_per_second: Some(2000.0),
            generated_tokens: 2,
            time_to_first_token_ms: Some(1),
            elapsed_ms: 2,
            tokens_per_second: 1000.0,
        })
    }
}

fn worker() -> (InferenceWorker, Arc<AtomicUsize>) {
    let releases = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&releases);
    (
        InferenceWorker::spawn(move || Mock {
            releases: counter,
            loaded: false,
        })
        .unwrap(),
        releases,
    )
}

#[test]
fn lifecycle_streaming_cancellation_and_cleanup() {
    let (worker, releases) = worker();
    let path = PathBuf::from("mock.gguf");
    assert_eq!(worker.status().unwrap().phase, Phase::Empty);
    assert!(!worker.cancel().unwrap());
    let failed = worker
        .load("rejected.gguf".into(), ModelConfig::default())
        .unwrap();
    assert_eq!(
        block_on(failed).unwrap().unwrap_err().code,
        ErrorCode::InvalidModel
    );
    assert_eq!(worker.status().unwrap().phase, Phase::Empty);
    assert_eq!(
        worker
            .generate(GenerationRequest::new("hi"), |_| Ok(()))
            .unwrap_err()
            .code,
        ErrorCode::NoModel
    );
    block_on(worker.load(path.clone(), ModelConfig::default()).unwrap())
        .unwrap()
        .unwrap();

    let (tx, rx) = mpsc::channel();
    let done = worker
        .generate(GenerationRequest::new("wait for stop"), move |delta| {
            tx.send(delta.text).unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "first");
    assert_eq!(worker.status().unwrap().phase, Phase::Generating);
    assert_eq!(
        worker
            .generate(GenerationRequest::new("second"), |_| Ok(()))
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(worker.unload().unwrap_err().code, ErrorCode::Busy);
    assert_eq!(
        worker
            .load(path.clone(), ModelConfig::default())
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert!(worker.cancel().unwrap());
    assert_eq!(
        block_on(done).unwrap().unwrap().reason,
        FinishReason::Cancelled
    );

    let (tx, rx) = mpsc::channel();
    let done = worker
        .generate(GenerationRequest::new("hi"), move |delta| {
            tx.send(delta.text).unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        block_on(done).unwrap().unwrap().reason,
        FinishReason::EndOfGeneration
    );
    assert_eq!(rx.into_iter().collect::<Vec<_>>(), ["first", " second"]);
    let failed = worker
        .generate(GenerationRequest::new("hi"), |_| {
            Err(InferenceError::new(ErrorCode::StreamClosed, "closed"))
        })
        .unwrap();
    assert_eq!(
        block_on(failed).unwrap().unwrap_err().code,
        ErrorCode::StreamClosed
    );
    assert_eq!(worker.status().unwrap().phase, Phase::Ready);

    block_on(worker.unload().unwrap()).unwrap().unwrap();
    assert_eq!(worker.status().unwrap().phase, Phase::Empty);
    assert_eq!(releases.load(Ordering::Relaxed), 1);
    block_on(worker.load(path, ModelConfig::default()).unwrap())
        .unwrap()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let pending = worker
        .generate(GenerationRequest::new("wait for stop"), move |delta| {
            tx.send(delta.text).unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "first");
    drop(worker);
    assert_eq!(
        block_on(pending).unwrap().unwrap().reason,
        FinishReason::Cancelled
    );
    assert_eq!(releases.load(Ordering::Relaxed), 2);
}
