use std::path::Path;

use agent_core::chat::ChatStore;
use agent_core::provider::{
    Budget, IntelligenceSettings, ProviderLocality, ProviderRetention, ProviderSettings, Sharing,
};

use super::*;

fn settings() -> ProviderSettings {
    ProviderSettings {
        name: "Fixture".into(),
        endpoint: "https://api.example.com/v1".into(),
        locality: ProviderLocality::Cloud,
        model: "fixture-model".into(),
        context_tokens: 8192,
        max_output_tokens: 1024,
        temperature: 0.7,
        image_input: false,
        stream_usage: true,
        requires_key: true,
        retention: ProviderRetention::ProviderPolicy,
        retention_summary: "Kept 30 days.".into(),
        sharing: Sharing::default(),
        pricing: None,
        budget: Budget::default(),
    }
}

fn store() -> SqliteStore {
    SqliteStore::open_test_fixture(Path::new(":memory:")).unwrap()
}

#[test]
fn providers_keep_their_key_and_selection_follows_deletion() {
    let mut db = store();
    assert_eq!(
        db.intelligence_settings().unwrap(),
        IntelligenceSettings::default()
    );
    let saved = db.save_provider(None, &settings()).unwrap();
    let mut changed = settings();
    changed.model = "other-model".into();
    let updated = db.save_provider(Some(saved.id), &changed).unwrap();
    assert_eq!(updated.key, saved.key);
    assert_eq!(updated.settings.model, "other-model");
    db.save_intelligence_settings(IntelligenceSettings {
        local_only: false,
        active_provider: Some(saved.id),
    })
    .unwrap();
    assert!(
        db.save_intelligence_settings(IntelligenceSettings {
            local_only: false,
            active_provider: Some(999),
        })
        .is_err()
    );
    db.begin_provider_deletion(saved.id).unwrap();
    db.finish_provider_deletion(saved.id).unwrap();
    assert_eq!(
        db.intelligence_settings().unwrap(),
        IntelligenceSettings {
            local_only: false,
            active_provider: None
        }
    );
    let mut invalid = settings();
    invalid.model = String::new();
    assert!(db.save_provider(None, &invalid).is_err());
    let stored: String = db
        .connection
        .query_row("SELECT settings FROM providers", [], |row| row.get(0))
        .unwrap_or_default();
    assert!(stored.is_empty());
}

#[test]
fn reservations_enforce_limits_and_settle_conservatively() {
    let mut db = store();
    let provider = db.save_provider(None, &settings()).unwrap().id;
    let limits = Limits {
        tokens: Some(1000),
        cost: None,
        unpriced_micros_per_million: None,
    };
    let reserve = |db: &mut SqliteStore, tokens| {
        db.reserve_usage(
            "2026-11",
            Reservation {
                provider,
                tokens,
                cost: None,
            },
            limits,
        )
    };
    let first = reserve(&mut db, 600).unwrap();
    // The in-flight reservation counts against the limit.
    assert_eq!(
        reserve(&mut db, 500).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    db.settle_usage(
        first,
        Settlement {
            transmission: LedgerTransmission::Sent,
            input_tokens: Some(100),
            output_tokens: Some(50),
            basis: LedgerBasis::ProviderReported,
            charged_tokens: 150,
            charged_cost: None,
            message: None,
        },
    )
    .unwrap();
    assert!(
        db.settle_usage(
            first,
            Settlement {
                transmission: LedgerTransmission::Sent,
                input_tokens: None,
                output_tokens: None,
                basis: LedgerBasis::None,
                charged_tokens: 0,
                charged_cost: None,
                message: None,
            },
        )
        .is_err()
    );
    let second = reserve(&mut db, 500).unwrap();
    let summary = db.usage_summary(provider, "2026-11").unwrap();
    assert_eq!(summary.reported_tokens, 150);
    assert_eq!(summary.reserved_tokens, 500);
    assert_eq!(summary.unpriced_requests, 2);
    // A different month has its own budget.
    assert!(
        db.reserve_usage(
            "2026-12",
            Reservation {
                provider,
                tokens: 900,
                cost: None
            },
            limits
        )
        .is_ok()
    );
    // Interrupted reservations are charged in full on reopen.
    super::settle_interrupted(&db.connection).unwrap();
    let summary = db.usage_summary(provider, "2026-11").unwrap();
    assert_eq!(summary.estimated_tokens, 500);
    assert_eq!(summary.reserved_tokens, 0);
    let _ = second;
}

#[test]
fn spending_limits_need_prices_and_count_unpriced_usage() {
    let mut db = store();
    let provider = db.save_provider(None, &settings()).unwrap().id;
    let unpriced = db
        .reserve_usage(
            "2026-11",
            Reservation {
                provider,
                tokens: 1_000_000,
                cost: None,
            },
            Limits::default(),
        )
        .unwrap();
    db.settle_usage(
        unpriced,
        Settlement {
            transmission: LedgerTransmission::Unknown,
            input_tokens: None,
            output_tokens: None,
            basis: LedgerBasis::Estimated,
            charged_tokens: 1_000_000,
            charged_cost: None,
            message: None,
        },
    )
    .unwrap();
    let spending = |price| Limits {
        tokens: None,
        cost: Some(5_000_000),
        unpriced_micros_per_million: price,
    };
    // Unknown price for the new request: refused.
    assert!(
        db.reserve_usage(
            "2026-11",
            Reservation {
                provider,
                tokens: 10,
                cost: None
            },
            spending(Some(1))
        )
        .is_err()
    );
    // Earlier unpriced usage without a fallback price: refused.
    assert!(
        db.reserve_usage(
            "2026-11",
            Reservation {
                provider,
                tokens: 10,
                cost: Some(1)
            },
            spending(None)
        )
        .is_err()
    );
    // 1M earlier tokens at $4/M = $4; a $0.50 request fits under $5, $1.50 does not.
    assert!(
        db.reserve_usage(
            "2026-11",
            Reservation {
                provider,
                tokens: 10,
                cost: Some(500_000)
            },
            spending(Some(4_000_000))
        )
        .is_ok()
    );
    assert!(
        db.reserve_usage(
            "2026-11",
            Reservation {
                provider,
                tokens: 10,
                cost: Some(600_000)
            },
            spending(Some(4_000_000))
        )
        .is_err()
    );
    assert_eq!(cost_micros(1, 1), 1);
    assert_eq!(cost_micros(2_000_000, 2_500_000), 5_000_000);
}

#[test]
fn schema_twelve_profiles_upgrade_local_only_and_keep_their_models() {
    let directory = std::env::temp_dir().join(format!(
        "agent-provider-upgrade-{}-{}",
        std::process::id(),
        crate::unix_ms().unwrap()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("chat.sqlite3");
    {
        let mut db = SqliteStore::open_test_fixture(&path).unwrap();
        let model = db
            .remember_model(
                Path::new("/models/lfm.gguf"),
                "lfm.gguf",
                agent_core::chat::ModelSettings::recommended(4096),
            )
            .unwrap();
        db.set_model_sha256(
            model.id,
            "b1b3de114215d9507409a662a501a631095a479a419584e8a2ded6304b19b4f5",
        )
        .unwrap();
        db.connection
            .execute_batch(
                "DROP TABLE web_research_settings; DROP TABLE model_downloads;
         ALTER TABLE personal_memories DROP COLUMN provenance; ALTER TABLE personal_memories DROP COLUMN classification;
         DROP TABLE provider_usage; DROP TABLE intelligence_settings; DROP TABLE providers;
                 ALTER TABLE models DROP COLUMN artifact_key; PRAGMA user_version = 12;",
            )
            .unwrap();
    }
    let db = SqliteStore::open_test_fixture(&path).unwrap();
    assert_eq!(
        db.intelligence_settings().unwrap(),
        IntelligenceSettings::default()
    );
    assert!(db.providers().unwrap().is_empty());
    let models = db.recent_models().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].file_name, "lfm.gguf");
    assert_eq!(
        models[0].artifact.as_deref(),
        Some("lfm2.5-1.2b-instruct-q4_k_m")
    );
    drop(db);
    let _ = std::fs::remove_dir_all(&directory);
}
