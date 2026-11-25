#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod bundled_model;
mod commands;
#[cfg(all(test, target_os = "linux"))]
mod desktop_acceptance;

use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64},
    },
};

use agent::*;
use commands::*;
use inference::{InferenceWorker, LlamaProvider};
use storage::SqliteStore;
use tauri::Manager;

fn prepare_database(path: &Path) -> std::io::Result<()> {
    let directory = path.parent().expect("database directory");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(error)
            if error.kind() == std::io::ErrorKind::AlreadyExists
                && path.symlink_metadata()?.is_file() =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn main() {
    run_desktop(tauri::Builder::default(), || {
        InferenceWorker::spawn(LlamaProvider::default)
    });
}

fn run_desktop(
    builder: tauri::Builder<tauri::Wry>,
    spawn_worker: impl FnOnce() -> agent_core::inference::Result<InferenceWorker> + Send + 'static,
) {
    builder
        .setup(move |app| {
            let path = app
                .path()
                .app_local_data_dir()?
                .join("chat/conversations.sqlite3");
            prepare_database(&path)?;
            let database = SqliteStore::open(&path)?;
            let worker = Arc::new(spawn_worker()?);
            let agent = Arc::new(
                AgentSession::new().map_err(|error| std::io::Error::other(error.message))?,
            );
            app.manage(Desktop {
                worker,
                agent,
                database: Arc::new(Mutex::new(database)),
                database_path: path,
                operations: Arc::new(AtomicBool::new(false)),
                active_model: AtomicI64::new(0),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            list_conversations,
            get_conversation,
            create_conversation,
            rename_conversation,
            set_system_prompt,
            delete_conversation,
            send_message,
            regenerate,
            continue_chat,
            chat_continuation_remaining,
            recommended_chat_settings,
            stop_generation,
            choose_model,
            load_model,
            save_model_settings,
            unload_model,
            save_preferences,
            inference_status,
            bundled_model::prepare_bundled_model,
            start_agent_task,
            start_agent_turn,
            agent::agent_conversation::get_unified_conversation,
            agent::agent_conversation::get_active_agent_turn,
            cancel_agent_task,
            respond_agent_approval,
            list_agent_tasks,
            get_agent_task,
            choose_agent_file,
            choose_agent_directory,
            list_agent_scopes,
            revoke_agent_scope
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the desktop application");
}
