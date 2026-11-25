//! A real desktop with a scripted inference worker and unchanged native permissions.
use std::{
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use agent_core::{
    agent::{DecisionRequest, DecisionResponse},
    chat::{ChatStore, ModelSettings},
    inference::{
        AssistantReplay, Cancellation, ErrorCode, FinishReason, GenerationRequest, GenerationStats,
        InferenceError, InferenceProvider, ModelConfig, ModelInfo, OutputBudget, Result, TextDelta,
    },
};
use serde_json::{Value, json};
use tauri::Manager;

use super::{InferenceWorker, run_desktop};

struct Fixture;

impl InferenceProvider for Fixture {
    fn load(&mut self, _: &Path, config: ModelConfig) -> Result<ModelInfo> {
        Ok(ModelInfo {
            name: "Deterministic fixture".into(),
            file_name: "fixture.gguf".into(),
            architecture: "fixture".into(),
            file_bytes: 0,
            parameters: 0,
            trained_context: 4096,
            supports_thinking: true,
            supports_agent: true,
            config,
            load_ms: 0,
        })
    }

    fn unload(&mut self) {}

    fn generate(
        &mut self,
        request: &GenerationRequest,
        cancellation: &Cancellation,
        emit: &mut dyn FnMut(TextDelta) -> Result<()>,
    ) -> Result<GenerationStats> {
        if request.prompt != "Fixture Chat continuation" {
            return Err(InferenceError::new(
                ErrorCode::GenerationFailed,
                "Unsupported synthetic prompt.",
            ));
        }
        if request.max_tokens == 4 {
            return Err(InferenceError::new(
                ErrorCode::ContextLimit,
                "Synthetic context limit.",
            ));
        }
        let earlier = request.chat.as_ref().and_then(|chat| chat.replay.as_ref());
        let mut generated = earlier.map_or_else(Vec::new, |replay| replay.generated.clone());
        let piece = match generated.len() {
            0 => "First ",
            1 => "second ",
            _ => "stopped.",
        };
        emit(TextDelta { text: piece.into() })?;
        generated.push(17);
        let reason = if request.max_tokens == 3 {
            let started = Instant::now();
            while !cancellation.is_cancelled() && started.elapsed() < Duration::from_secs(4) {
                thread::sleep(Duration::from_millis(10));
            }
            if cancellation.is_cancelled() {
                FinishReason::Cancelled
            } else {
                FinishReason::EndOfGeneration
            }
        } else {
            FinishReason::TokenLimit
        };
        Ok(GenerationStats {
            reason,
            prompt_tokens: 10,
            prompt_tokens_per_second: None,
            generated_tokens: 1,
            time_to_first_token_ms: Some(1),
            elapsed_ms: 2,
            tokens_per_second: 1.0,
            excluded_turns: 0,
            output_budget: Some(OutputBudget {
                requested: request.max_tokens,
                effective: request.max_tokens,
                context_length: 2048,
            }),
            assistant_replay: (reason == FinishReason::TokenLimit).then_some(AssistantReplay {
                prompt_fingerprint: 42,
                generated,
                utf8_tail: Vec::new(),
                excluded_turns: 0,
            }),
        })
    }

    fn decide(
        &mut self,
        request: &DecisionRequest,
        cancellation: &Cancellation,
    ) -> Result<DecisionResponse> {
        let generation = request.generation_request()?;
        let payload: Value = serde_json::from_str(generation.prompt.lines().next().unwrap())
            .expect("decision fixture payload");
        let steps = payload["previous_steps"]
            .as_array()
            .expect("decision fixture steps");
        let next = match (payload["user_task"].as_str(), steps.len()) {
            (Some("Fixture calculator"), 0) => {
                json!({"kind":"tool_call","tool":"math.calculate","arguments":{"expression":"2+3*4"}})
            }
            (Some("Fixture calculator"), 1) => {
                assert_eq!(steps[0]["untrusted_observation"]["result"]["value"], 14.0);
                json!({"kind":"final","answer":"The calculator returned 14."})
            }
            (Some("Fixture calculator follow-up"), 0) => {
                assert!(
                    generation
                        .history
                        .turns
                        .last()
                        .unwrap()
                        .assistant
                        .contains("14")
                );
                json!({"kind":"final","answer":"The previous result was 14."})
            }
            (Some("Fixture KCalc approval"), 0) => {
                json!({"kind":"tool_call","tool":"apps.list","arguments":{}})
            }
            (Some("Fixture KCalc approval"), 1) => {
                let applications = steps[0]["untrusted_observation"]["result"]["applications"]
                    .as_array()
                    .expect("native application catalog");
                match applications
                    .iter()
                    .find(|app| app["display_name"] == "KCalc")
                {
                    Some(app) => {
                        json!({"kind":"tool_call","tool":"apps.launch","arguments":{"app_id":app["app_id"]}})
                    }
                    None => {
                        json!({"kind":"final","answer":"KCalc is unavailable in the trusted catalog."})
                    }
                }
            }
            (Some("Fixture KCalc approval"), 2) => {
                assert_eq!(
                    steps[1]["untrusted_observation"]["result"]["dispatched"],
                    true
                );
                json!({"kind":"final","answer":"KCalc dispatch accepted."})
            }
            (Some("Fixture cancellation"), 0) => {
                let start = Instant::now();
                while !cancellation.is_cancelled() && start.elapsed() < Duration::from_secs(12) {
                    thread::sleep(Duration::from_millis(10));
                }
                json!({"kind":"final","answer":"Fixture decision ended."})
            }
            _ => json!({"kind":"final","answer":"Unknown fixture."}),
        };
        Ok(DecisionResponse {
            text: next.to_string(),
            stats: GenerationStats {
                reason: if cancellation.is_cancelled() {
                    FinishReason::Cancelled
                } else {
                    FinishReason::EndOfGeneration
                },
                prompt_tokens: 1,
                prompt_tokens_per_second: None,
                generated_tokens: 1,
                time_to_first_token_ms: None,
                elapsed_ms: 1,
                tokens_per_second: 1.0,
                excluded_turns: 0,
                output_budget: None,
                assistant_replay: None,
            },
        })
    }
}

fn isolated_profile() -> PathBuf {
    let profile = PathBuf::from(
        std::env::var_os("AGENT_ACCEPTANCE_PROFILE").expect("temporary acceptance profile"),
    )
    .canonicalize()
    .unwrap();
    assert!(profile.starts_with(std::env::temp_dir()) && profile != std::env::temp_dir());
    let data = PathBuf::from(std::env::var_os("XDG_DATA_HOME").expect("temporary data directory"))
        .canonicalize()
        .unwrap();
    assert_eq!(data, profile.join("data"));
    profile
}

fn fixture_worker() -> Result<InferenceWorker> {
    let worker = InferenceWorker::spawn(|| Fixture)?;
    let pending = worker.load(PathBuf::from("fixture.gguf"), ModelConfig::default())?;
    tauri::async_runtime::block_on(pending).expect("fixture inference worker")?;
    Ok(worker)
}

#[test]
#[ignore = "interactive Linux desktop fixture; use an isolated profile and X11 session"]
fn desktop_approval_fixture() {
    isolated_profile();
    run_desktop(tauri::Builder::default().any_thread(), fixture_worker);
}

#[test]
#[ignore = "launches a deterministic packaged WebView under an isolated X11 profile"]
fn unified_ipc_desktop_fixture() {
    let profile = isolated_profile();
    let launched = std::sync::atomic::AtomicBool::new(false);
    let builder = tauri::Builder::default()
        .any_thread()
        .on_page_load(move |webview, page| {
            if page.event() != tauri::webview::PageLoadEvent::Finished
                || launched.swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                return;
            }
            {
                let desktop = webview.state::<crate::commands::Desktop>();
                let model = desktop
                    .database
                    .lock()
                    .unwrap()
                    .remember_model(
                        Path::new("/synthetic/fixture.gguf"),
                        "fixture.gguf",
                        ModelSettings::default(),
                    )
                    .unwrap();
                desktop
                    .active_model
                    .store(model.id, std::sync::atomic::Ordering::Release);
            }
            webview
                .eval(include_str!("unified_ipc_acceptance.js"))
                .unwrap();
            let result_path = profile.join("ipc-result.json");
            let webview = (*webview).clone();
            thread::spawn(move || {
                for _ in 0..120 {
                    let (tx, rx) = std::sync::mpsc::sync_channel(1);
                    let sent = webview.eval_with_callback(
                        "document.documentElement.dataset.unifiedIpcResult || ''",
                        move |raw| {
                            let _ = tx.send(raw);
                        },
                    );
                    if sent.is_err() {
                        break;
                    }
                    if let Ok(raw) = rx.recv_timeout(Duration::from_millis(500)) {
                        let result: Option<Value> = serde_json::from_str::<String>(&raw)
                            .ok()
                            .and_then(|text| serde_json::from_str(&text).ok());
                        if let Some(result) = result {
                            std::fs::write(&result_path, result.to_string()).unwrap();
                            webview.app_handle().exit(i32::from(result["pass"] != true));
                            return;
                        }
                    }
                    thread::sleep(Duration::from_millis(200));
                }
                std::fs::write(
                    &result_path,
                    r#"{"pass":false,"error":"fixture timed out"}"#,
                )
                .unwrap();
                webview.app_handle().exit(1);
            });
        });
    run_desktop(builder, fixture_worker);
}
