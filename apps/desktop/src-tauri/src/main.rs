#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod bundled_model;
mod chat_context;
mod commands;
#[cfg(all(test, target_os = "linux"))]
mod desktop_acceptance;
mod knowledge;
mod platform;
mod semantic;

use std::{path::Path, sync::Arc};

use agent::*;
use commands::*;
use engine::Engine;
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
            let engine = Arc::new(Engine::new(
                Arc::new(platform::TauriPlatform),
                worker,
                database,
                path,
            )?);
            let agent = Arc::new(
                AgentSession::new().map_err(|error| std::io::Error::other(error.message))?,
            );
            app.manage(Desktop { engine, agent });
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
            list_models,
            load_model,
            save_model_settings,
            reset_model_settings,
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
            revoke_agent_scope,
            knowledge::knowledge_start_import,
            knowledge::knowledge_reimport,
            knowledge::knowledge_import_status,
            knowledge::knowledge_cancel_import,
            knowledge::knowledge_list_documents,
            chat_context::chat_supplied_evidence,
            chat_context::chat_resolve_evidence,
            chat_context::memory_page,
            knowledge::knowledge_library,
            knowledge::knowledge_get_document,
            knowledge::knowledge_delete_document,
            knowledge::knowledge_search,
            semantic::knowledge_embedding_status,
            semantic::knowledge_choose_embedding_model,
            semantic::knowledge_unload_embedding_model,
            semantic::knowledge_embedding_coverage,
            semantic::knowledge_reindex,
            semantic::knowledge_index_progress,
            semantic::knowledge_cancel_index,
            semantic::knowledge_retrieve,
            chat_context::chat_source_references,
            chat_context::chat_resolve_source,
            chat_context::memory_retrieval_enabled,
            chat_context::memory_set_retrieval_enabled,
            chat_context::memory_create,
            chat_context::memory_list,
            chat_context::memory_edit,
            chat_context::memory_delete,
            chat_context::memory_clear,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the desktop application");
}
