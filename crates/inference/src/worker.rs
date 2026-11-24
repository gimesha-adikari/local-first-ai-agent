use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, mpsc},
    thread::{self, JoinHandle},
};

use agent_core::inference::{
    Cancellation, ErrorCode, GenerationRequest, GenerationStats, InferenceError, InferenceProvider,
    ModelConfig, ModelInfo, Result, TextDelta,
};
use futures_channel::oneshot;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Empty,
    Loading,
    Ready,
    Generating,
    Unloading,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub model: Option<ModelInfo>,
}

struct State {
    status: Status,
    cancellation: Option<Cancellation>,
}

type Job = Box<dyn FnOnce(&mut dyn InferenceProvider, &Mutex<State>) + Send>;
pub type Pending<T> = oneshot::Receiver<Result<T>>;

/// Reserves one operation at submission and executes it on a dedicated thread.
pub struct InferenceWorker {
    sender: Option<mpsc::Sender<Job>>,
    state: Arc<Mutex<State>>,
    thread: Option<JoinHandle<()>>,
}

fn stopped() -> InferenceError {
    InferenceError::new(ErrorCode::WorkerStopped, "The inference worker stopped.")
}

impl InferenceWorker {
    pub fn spawn<P, F>(create: F) -> Result<Self>
    where
        P: InferenceProvider,
        F: FnOnce() -> P + Send + 'static,
    {
        let state = Arc::new(Mutex::new(State {
            status: Status {
                phase: Phase::Empty,
                model: None,
            },
            cancellation: None,
        }));
        let (tx, rx) = mpsc::channel::<Job>();
        let worker_state = Arc::clone(&state);
        let thread = thread::Builder::new()
            .name("local-inference".into())
            .spawn(move || {
                let mut provider = create();
                for job in rx {
                    job(&mut provider, &worker_state);
                }
                provider.unload();
            })
            .map_err(|_| stopped())?;
        Ok(Self {
            sender: Some(tx),
            state,
            thread: Some(thread),
        })
    }

    fn state(&self) -> Result<MutexGuard<'_, State>> {
        if self
            .thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
        {
            return Err(stopped());
        }
        self.state.lock().map_err(|_| stopped())
    }

    pub fn status(&self) -> Result<Status> {
        Ok(self.state()?.status.clone())
    }

    pub fn load(&self, path: PathBuf, config: ModelConfig) -> Result<Pending<ModelInfo>> {
        config.validate()?;
        let state = self.reserve(Phase::Loading, Phase::Empty)?;
        self.submit(
            state,
            Phase::Loading,
            None,
            move |provider| provider.load(&path, config),
            |state, result| state.status.model = result.as_ref().ok().cloned(),
        )
    }

    pub fn unload(&self) -> Result<Pending<()>> {
        let state = self.reserve(Phase::Unloading, Phase::Ready)?;
        self.submit(
            state,
            Phase::Unloading,
            None,
            |provider| {
                provider.unload();
                Ok(())
            },
            |state, _| state.status.model = None,
        )
    }

    pub fn generate(
        &self,
        request: GenerationRequest,
        mut emit: impl FnMut(TextDelta) -> Result<()> + Send + 'static,
    ) -> Result<Pending<GenerationStats>> {
        request.validate()?;
        let state = self.reserve(Phase::Generating, Phase::Ready)?;
        let cancellation = Cancellation::default();
        let worker_cancellation = cancellation.clone();
        self.submit(
            state,
            Phase::Generating,
            Some(cancellation),
            move |provider| provider.generate(&request, &worker_cancellation, &mut emit),
            |_, _| {},
        )
    }

    pub fn cancel(&self) -> Result<bool> {
        let state = self.state()?;
        if let Some(cancellation) = &state.cancellation {
            cancellation.cancel();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn reserve(&self, next: Phase, required: Phase) -> Result<MutexGuard<'_, State>> {
        let state = self.state()?;
        if state.status.phase == required {
            return Ok(state);
        }
        match (state.status.phase, next) {
            (Phase::Empty, _) => Err(InferenceError::new(
                ErrorCode::NoModel,
                "Load a GGUF model first.",
            )),
            (Phase::Ready, Phase::Loading) => Err(InferenceError::new(
                ErrorCode::Busy,
                "Unload the current model first.",
            )),
            _ => Err(InferenceError::new(
                ErrorCode::Busy,
                "Wait for the current inference operation to finish.",
            )),
        }
    }

    fn submit<T: Send + 'static>(
        &self,
        mut state: MutexGuard<'_, State>,
        phase: Phase,
        cancellation: Option<Cancellation>,
        run: impl FnOnce(&mut dyn InferenceProvider) -> Result<T> + Send + 'static,
        complete: impl FnOnce(&mut State, &Result<T>) + Send + 'static,
    ) -> Result<Pending<T>> {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move |provider, shared| {
            let result = run(provider);
            let result = match shared.lock() {
                Ok(mut state) => {
                    complete(&mut state, &result);
                    state.status.phase = if state.status.model.is_some() {
                        Phase::Ready
                    } else {
                        Phase::Empty
                    };
                    state.cancellation = None;
                    result
                }
                Err(_) => Err(stopped()),
            };
            let _ = tx.send(result);
        });
        self.sender
            .as_ref()
            .ok_or_else(stopped)?
            .send(job)
            .map_err(|_| stopped())?;
        state.status.phase = phase;
        state.cancellation = cancellation;
        Ok(rx)
    }
}

impl Drop for InferenceWorker {
    fn drop(&mut self) {
        let _ = self.cancel();
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
