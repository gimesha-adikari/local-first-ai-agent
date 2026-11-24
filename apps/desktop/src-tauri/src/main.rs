#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use agent_core::inference::{
    ErrorCode, GenerationRequest, GenerationStats, InferenceError, ModelConfig, ModelInfo, Result,
    TextDelta,
};
use inference::{InferenceWorker, LlamaProvider, Pending, Phase, Status};
use tauri::{Manager, State, async_runtime::Mutex, ipc::Channel};

struct DesktopInference {
    worker: InferenceWorker,
    selected: Mutex<Option<PathBuf>>,
}

fn picker_busy() -> InferenceError {
    InferenceError::new(ErrorCode::Busy, "The model picker is open.")
}

#[tauri::command]
async fn choose_model(state: State<'_, DesktopInference>) -> Result<Option<String>> {
    let mut selected = state.selected.try_lock().map_err(|_| picker_busy())?;
    if state.worker.status()?.phase != Phase::Empty {
        return Err(InferenceError::new(
            ErrorCode::Busy,
            "Unload the current model before choosing another.",
        ));
    }
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_title("Choose a GGUF model")
        .add_filter("GGUF models", &["gguf"])
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    let path = file.path();
    let name = path
        .file_name()
        .ok_or_else(|| {
            InferenceError::new(
                ErrorCode::PickerFailed,
                "The picker did not return a file name.",
            )
        })?
        .to_string_lossy()
        .into_owned();
    *selected = Some(path.to_path_buf());
    Ok(Some(name))
}

#[tauri::command]
fn inference_status(state: State<'_, DesktopInference>) -> Result<Status> {
    state.worker.status()
}

async fn wait_for<T>(pending: Pending<T>) -> Result<T> {
    pending.await.map_err(|_| {
        InferenceError::new(ErrorCode::WorkerStopped, "The inference worker stopped.")
    })?
}

#[tauri::command]
async fn load_model(state: State<'_, DesktopInference>, config: ModelConfig) -> Result<ModelInfo> {
    let pending = {
        let selected = state.selected.try_lock().map_err(|_| picker_busy())?;
        let path = selected.clone().ok_or_else(|| {
            InferenceError::new(ErrorCode::NoSelection, "Choose a GGUF model first.")
        })?;
        state.worker.load(path, config)?
    };
    wait_for(pending).await
}

#[tauri::command]
async fn unload_model(state: State<'_, DesktopInference>) -> Result<()> {
    wait_for(state.worker.unload()?).await
}

#[tauri::command]
async fn generate(
    state: State<'_, DesktopInference>,
    request: GenerationRequest,
    tokens: Channel<TextDelta>,
) -> Result<GenerationStats> {
    let pending = state.worker.generate(request, move |delta| {
        tokens.send(delta).map_err(|_| {
            InferenceError::new(
                ErrorCode::StreamClosed,
                "The generation output stream closed.",
            )
        })
    })?;
    wait_for(pending).await
}

#[tauri::command]
fn stop_generation(state: State<'_, DesktopInference>) -> Result<bool> {
    state.worker.cancel()
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            app.manage(DesktopInference {
                worker: InferenceWorker::spawn(LlamaProvider::default)?,
                selected: Mutex::new(None),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            choose_model,
            inference_status,
            load_model,
            unload_model,
            generate,
            stop_generation
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the desktop application");
}
