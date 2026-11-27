#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod bundled_model;
mod capture;
mod chat_context;
mod commands;
mod credentials;
mod database_keyring;
mod database_startup;
#[cfg(all(test, target_os = "linux"))]
mod desktop_acceptance;
mod knowledge;
mod panel;
mod platform;
mod portal_bus;
mod providers;
mod semantic;
mod summon;

use std::{path::Path, sync::Arc};

use agent::*;
use commands::*;
use engine::Engine;
use inference::{InferenceWorker, LlamaProvider};
use storage::SqliteStore;
use tauri::Manager;

fn main() {
    let instance = match summon::claim() {
        Ok(Some(instance)) => instance,
        Ok(None) => return,
        Err(error) => {
            eprintln!("Could not start the desktop application: {error}");
            std::process::exit(1)
        }
    };
    run_desktop_instance(
        tauri::Builder::default(),
        || InferenceWorker::spawn(LlamaProvider::default),
        Some(instance),
        database_startup::open_native,
    );
}

#[cfg(test)]
fn run_desktop(
    builder: tauri::Builder<tauri::Wry>,
    spawn_worker: impl FnOnce() -> agent_core::inference::Result<InferenceWorker> + Send + 'static,
) {
    run_desktop_instance(builder, spawn_worker, None, database_startup::open_fixture)
}

fn run_desktop_instance(
    builder: tauri::Builder<tauri::Wry>,
    spawn_worker: impl FnOnce() -> agent_core::inference::Result<InferenceWorker> + Send + 'static,
    instance: Option<summon::Instance>,
    open_database: fn(&Path) -> Option<(SqliteStore, database_startup::DatabaseAccess)>,
) {
    builder
        .register_uri_scheme_protocol("glance-crop", panel::crop_protocol)
        .setup(move |app| {
            let path = app
                .path()
                .app_local_data_dir()?
                .join("chat/conversations.sqlite3");
            let Some((database, access)) = open_database(&path) else {
                app.handle().exit(1);
                return Ok(());
            };
            let worker = Arc::new(spawn_worker()?);
            let engine = Arc::new(Engine::with_network(
                Arc::new(platform::TauriPlatform),
                worker,
                database,
                path,
                Arc::new(credentials::SecretServiceStore),
                engine::network::BoundaryConfig::default(),
            )?);
            let agent = Arc::new(
                AgentSession::new().map_err(|error| std::io::Error::other(error.message))?,
            );
            app.manage(Desktop { engine, agent });
            app.manage(panel::Panel::default());
            app.manage(access);
            let main_config = app
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == "main")
                .expect("main window configuration");
            tauri::WebviewWindowBuilder::from_config(app, main_config)?.build()?;
            panel::prewarm(app.handle());
            summon::install(app.handle(), instance);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            summon::shortcut_status,
            summon::configure_shortcut,
            panel::glance_show,
            panel::glance_state,
            panel::glance_clipboard,
            panel::glance_run,
            panel::glance_stop,
            panel::glance_copy,
            panel::glance_remember,
            panel::glance_dismiss,
            panel::glance_open_main,
            panel::screen::glance_capture,
            panel::screen::glance_capture_clear,
            panel::screen::glance_ocr,
            panel::screen::glance_estimate,
            bootstrap,
            database_startup::database_recovery_key,
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
            providers::provider_list,
            providers::provider_save,
            providers::provider_delete,
            providers::provider_test,
            providers::provider_enter_credential,
            providers::provider_delete_credential,
            providers::intelligence_settings,
            providers::set_intelligence_settings,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the desktop application");
}
