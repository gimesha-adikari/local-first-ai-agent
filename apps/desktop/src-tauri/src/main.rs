#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

use std::{
    path::Path,
    sync::{Arc, Mutex, atomic::AtomicI64},
};

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
    tauri::Builder::default()
        .setup(|app| {
            let path = app
                .path()
                .app_local_data_dir()?
                .join("chat/conversations.sqlite3");
            prepare_database(&path)?;
            let database = SqliteStore::open(&path)?;
            app.manage(Desktop {
                worker: InferenceWorker::spawn(LlamaProvider::default)?,
                database: Arc::new(Mutex::new(database)),
                database_path: path,
                operations: tauri::async_runtime::Mutex::new(()),
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
            stop_generation,
            choose_model,
            load_model,
            save_model_settings,
            unload_model,
            save_preferences,
            inference_status
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the desktop application");
}
