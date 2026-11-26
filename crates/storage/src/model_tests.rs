//! Task 06-A: model identity, automatic settings and remembered runtime state.

use std::path::Path;

use agent_core::chat::{ChatStore, ModelSettings};

use super::*;

#[test]
fn schema_ten_classifies_recommended_rows_as_automatic() {
    let database = TestDatabase::new();
    let mut store = database.open();
    let automatic = store
        .remember_model(
            Path::new("/m/auto.gguf"),
            "auto.gguf",
            ModelSettings::recommended(8192),
        )
        .unwrap();
    let mut custom_settings = ModelSettings::recommended(2048);
    custom_settings.max_tokens = 768;
    let custom = store
        .remember_model(Path::new("/m/custom.gguf"), "custom.gguf", custom_settings)
        .unwrap();
    store
        .connection
        .execute_batch(
            "DROP TABLE intelligence_receipts; ALTER TABLE messages DROP COLUMN classification;
             DROP TABLE chat_route_coverage; ALTER TABLE models DROP COLUMN content_sha256;
             DROP TABLE model_runtime; ALTER TABLE models DROP COLUMN settings_mode;
             ALTER TABLE models DROP COLUMN content_key; PRAGMA user_version=9;",
        )
        .unwrap();
    drop(store);
    let store = database.open();
    let models = store.recent_models().unwrap();
    let mode = |id| {
        models
            .iter()
            .find(|model| model.id == id)
            .unwrap()
            .automatic_settings
    };
    assert!(mode(automatic.id));
    assert!(!mode(custom.id));
    assert_eq!(
        store.model(custom.id).unwrap().reference.settings,
        custom_settings
    );
    assert!(!store.chat_model_unloaded().unwrap());
    assert_eq!(store.embedding_model_choice().unwrap(), None);
}

#[test]
fn copies_of_the_same_weights_collapse_into_the_bundled_row_and_keep_custom_settings() {
    let mut store = in_memory();
    let mut custom = ModelSettings::recommended(4096);
    custom.temperature = 0.2;
    let imported = store
        .remember_model(
            Path::new("/backup/lfm.gguf"),
            "lfm.gguf",
            ModelSettings::recommended(8192),
        )
        .unwrap();
    store
        .set_model_settings(imported.id, custom, false)
        .unwrap();
    let other = store
        .remember_model(
            Path::new("/m/qwen.gguf"),
            "qwen.gguf",
            ModelSettings::recommended(8192),
        )
        .unwrap();
    let bundled = store
        .register_bundled_model(
            "lfm",
            Path::new("/app/lfm.gguf"),
            "lfm.gguf",
            ModelSettings::recommended(8192),
        )
        .unwrap()
        .reference;
    assert!(bundled.bundled && bundled.automatic_settings);
    assert_eq!(store.recent_models().unwrap().len(), 3);

    for (id, key) in [
        (imported.id, "s1:10:aa"),
        (bundled.id, "s1:10:aa"),
        (other.id, "s1:20:bb"),
    ] {
        store.set_model_content_key(id, key).unwrap();
    }
    assert_eq!(store.models_without_content_key().unwrap(), []);
    // A sampled match alone only nominates candidates.
    let mut candidates: Vec<_> = store
        .duplicate_candidates()
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    candidates.sort();
    assert_eq!(candidates, [imported.id, bundled.id]);
    assert_eq!(store.merge_duplicate_models().unwrap(), 0);
    assert_eq!(store.recent_models().unwrap().len(), 3);
    // Different full digests keep both rows listed.
    store
        .set_model_sha256(imported.id, &"b".repeat(64))
        .unwrap();
    store.set_model_sha256(bundled.id, &"a".repeat(64)).unwrap();
    assert_eq!(store.merge_duplicate_models().unwrap(), 0);
    assert_eq!(store.recent_models().unwrap().len(), 3);
    store
        .set_model_sha256(imported.id, &"A".repeat(64))
        .unwrap();
    assert_eq!(store.merge_duplicate_models().unwrap(), 1);
    let listed = store.recent_models().unwrap();
    let ids: Vec<_> = listed.iter().map(|model| model.id).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&bundled.id) && ids.contains(&other.id));
    let survivor = listed.iter().find(|model| model.id == bundled.id).unwrap();
    assert!(survivor.bundled);
    assert!(!survivor.automatic_settings, "custom settings were adopted");
    assert_eq!(survivor.settings, custom);
    // The hidden row, its path and settings remain intact.
    let hidden = store.model(imported.id).unwrap();
    assert_eq!(hidden.path, Path::new("/backup/lfm.gguf"));
    assert!(!hidden.reference.bundled);
    assert_eq!(
        store
            .model_with_sha256(&"a".repeat(64))
            .unwrap()
            .unwrap()
            .id,
        bundled.id
    );
    assert_eq!(
        store
            .model_with_sha256(&"c".repeat(64))
            .unwrap()
            .map(|m| m.id),
        None
    );
    // Merging again is a no-op.
    assert_eq!(store.merge_duplicate_models().unwrap(), 1);
    assert_eq!(store.model(bundled.id).unwrap().reference.settings, custom);
}

#[test]
fn settings_mode_and_runtime_choices_persist() {
    let database = TestDatabase::new();
    let mut store = database.open();
    let model = store
        .remember_model(
            Path::new("/m/a.gguf"),
            "a.gguf",
            ModelSettings::recommended(4096),
        )
        .unwrap();
    assert!(model.automatic_settings);
    store
        .set_model_settings(model.id, ModelSettings::recommended(2048), false)
        .unwrap();
    store.set_chat_model_unloaded(true).unwrap();
    store
        .remember_embedding_model(Path::new("/m/bge-small-en-v1.5-f16.gguf"))
        .unwrap();
    drop(store);
    let mut store = database.open();
    let saved = store.model(model.id).unwrap().reference;
    assert!(!saved.automatic_settings);
    assert_eq!(saved.settings, ModelSettings::recommended(2048));
    assert!(store.chat_model_unloaded().unwrap());
    assert_eq!(
        store.embedding_model_choice().unwrap(),
        Some((
            Path::new("/m/bge-small-en-v1.5-f16.gguf").to_path_buf(),
            true
        ))
    );
    store.set_embedding_restore(false).unwrap();
    assert!(!store.embedding_model_choice().unwrap().unwrap().1);
    store
        .set_model_settings(model.id, ModelSettings::recommended(4096), true)
        .unwrap();
    assert!(store.model(model.id).unwrap().reference.automatic_settings);
    store.forget_embedding_model().unwrap();
    assert_eq!(store.embedding_model_choice().unwrap(), None);
    assert!(store.set_model_content_key(model.id, "").is_err());
}

#[test]
fn last_loaded_model_restores_unless_explicitly_unloaded() {
    let database = TestDatabase::new();
    let mut store = database.open();
    assert!(store.chat_model_to_restore().unwrap().is_none());
    let model = store
        .remember_model(
            Path::new("/m/qwen.gguf"),
            "qwen.gguf",
            ModelSettings::recommended(8192),
        )
        .unwrap();
    let copy = store
        .remember_model(
            Path::new("/backup/qwen.gguf"),
            "qwen.gguf",
            ModelSettings::recommended(8192),
        )
        .unwrap();
    store.set_last_chat_model(copy.id).unwrap();
    drop(store);
    let mut store = database.open();
    let restore = store.chat_model_to_restore().unwrap().unwrap();
    assert_eq!(restore.reference.id, copy.id);
    assert_eq!(restore.path, Path::new("/backup/qwen.gguf"));
    // A consolidated copy restores through the listed row.
    store.set_model_sha256(model.id, &"d".repeat(64)).unwrap();
    store.set_model_sha256(copy.id, &"d".repeat(64)).unwrap();
    store.mark_model_used(model.id).unwrap();
    store.merge_duplicate_models().unwrap();
    let listed: Vec<_> = store
        .recent_models()
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        store.chat_model_to_restore().unwrap().unwrap().reference.id,
        listed[0]
    );
    store.set_chat_model_unloaded(true).unwrap();
    assert!(store.chat_model_to_restore().unwrap().is_none());
    store.set_last_chat_model(model.id).unwrap();
    assert!(!store.chat_model_unloaded().unwrap());
}
