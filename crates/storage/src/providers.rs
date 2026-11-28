//! Provider configuration, intelligence selection and the usage ledger.

use agent_core::{
    inference::{ErrorCode, InferenceError, Result},
    provider::{IntelligenceSettings, MAX_PROVIDERS, ProviderId, ProviderSettings},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::{SqliteStore, not_found, sql_error, storage_error, unix_ms};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedProvider {
    pub id: ProviderId,
    /// Stable identity for destinations and secure storage.
    pub key: String,
    pub settings: ProviderSettings,
    pub updated_at: i64,
    pub deletion_pending: bool,
    pub obsolete_credential_origin: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerTransmission {
    Unsent,
    Sent,
    Unknown,
}

impl LedgerTransmission {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unsent => "unsent",
            Self::Sent => "sent",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerBasis {
    ProviderReported,
    Estimated,
    None,
}

impl LedgerBasis {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProviderReported => "provider_reported",
            Self::Estimated => "estimated",
            Self::None => "none",
        }
    }
}

/// A reservation to admit.
#[derive(Debug, Clone, Copy)]
pub struct Reservation {
    pub provider: ProviderId,
    pub tokens: u64,
    /// `None` when pricing is unknown.
    pub cost: Option<u64>,
}

/// Configured limits checked against the period's reserved and charged
/// totals in the same transaction as the new reservation.
#[derive(Debug, Clone, Copy, Default)]
pub struct Limits {
    pub tokens: Option<u64>,
    pub cost: Option<u64>,
    /// Highest configured price per million tokens, applied to earlier
    /// usage that was recorded without a price.
    pub unpriced_micros_per_million: Option<u64>,
}

/// Cost in millionths of the currency, rounded up.
pub fn cost_micros(tokens: u64, micros_per_million: u64) -> u64 {
    let product = u128::from(tokens) * u128::from(micros_per_million);
    u64::try_from(product.div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy)]
pub struct Settlement {
    pub transmission: LedgerTransmission,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub basis: LedgerBasis,
    pub charged_tokens: u64,
    pub charged_cost: Option<u64>,
    pub message: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub requests: u64,
    /// Tokens the provider reported.
    pub reported_tokens: u64,
    /// Tokens charged from application estimates because no report arrived.
    pub estimated_tokens: u64,
    /// Tokens held by requests still in flight.
    pub reserved_tokens: u64,
    /// Estimated cost from configured prices, in millionths of the currency.
    pub cost_micros: u64,
    /// Requests whose cost could not be estimated because prices are unknown.
    pub unpriced_requests: u64,
}

fn budget_exceeded(message: &str) -> InferenceError {
    InferenceError::new(ErrorCode::BudgetExceeded, message)
}

impl SavedProvider {
    pub fn require_available(&self) -> Result<()> {
        if self.deletion_pending {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "This provider is being removed. Retry removal in Providers.",
            ));
        }
        Ok(())
    }
}

fn decode(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedProvider> {
    let settings: String = row.get(2)?;
    Ok(SavedProvider {
        id: row.get(0)?,
        key: row.get(1)?,
        settings: serde_json::from_str(&settings).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        updated_at: row.get(3)?,
        deletion_pending: row.get(4)?,
        obsolete_credential_origin: row.get(5)?,
    })
}

impl SqliteStore {
    pub fn providers(&self) -> Result<Vec<SavedProvider>> {
        self.connection
            .prepare(
                "SELECT id, key, settings, updated_at, deletion_pending, obsolete_credential_origin FROM providers ORDER BY id",
            )
            .map_err(sql_error)?
            .query_map([], decode)
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)
    }

    pub fn provider(&self, id: ProviderId) -> Result<SavedProvider> {
        self.connection
            .query_row(
                "SELECT id, key, settings, updated_at, deletion_pending, obsolete_credential_origin FROM providers WHERE id = ?1",
                [id],
                decode,
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(not_found)
    }

    /// Insert or update validated settings. The engine canonicalizes the
    /// endpoint before calling this.
    pub fn save_provider(
        &mut self,
        id: Option<ProviderId>,
        settings: &ProviderSettings,
    ) -> Result<SavedProvider> {
        self.save_provider_with_key_cleanup(id, settings, None)
    }

    pub fn save_provider_with_key_cleanup(
        &mut self,
        id: Option<ProviderId>,
        settings: &ProviderSettings,
        obsolete_origin: Option<&str>,
    ) -> Result<SavedProvider> {
        settings.validate()?;
        let encoded = serde_json::to_string(settings).map_err(|_| storage_error())?;
        let now = unix_ms()?;
        let transaction = self.connection.transaction().map_err(sql_error)?;
        let id = match id {
            Some(id) => {
                let changed = transaction
                    .execute(
                        "UPDATE providers SET settings = ?1, updated_at = ?2, obsolete_credential_origin = ?4
                         WHERE id = ?3 AND deletion_pending = 0 AND obsolete_credential_origin IS NULL",
                        params![encoded, now, id, obsolete_origin],
                    )
                    .map_err(sql_error)?;
                if changed == 0 {
                    return Err(not_found());
                }
                id
            }
            None => {
                let count: i64 = transaction
                    .query_row("SELECT count(*) FROM providers", [], |row| row.get(0))
                    .map_err(sql_error)?;
                if count as usize >= MAX_PROVIDERS {
                    return Err(InferenceError::new(
                        ErrorCode::InvalidConfig,
                        "Remove a provider before adding another.",
                    ));
                }
                let next: i64 = transaction
                    .query_row(
                        "SELECT coalesce(max(id), 0) + 1 FROM providers",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(sql_error)?;
                // The key never changes, so secure storage and destinations
                // stay bound to this provider across edits.
                let key = format!("p{next}-{now:x}");
                transaction
                    .execute(
                        "INSERT INTO providers (id, key, settings, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?4)",
                        params![next, key, encoded, now],
                    )
                    .map_err(sql_error)?;
                next
            }
        };
        transaction.commit().map_err(sql_error)?;
        self.provider(id)
    }

    /// Disable the provider durably before changing its registration or key.
    pub fn finish_provider_key_cleanup(&mut self, id: ProviderId) -> Result<()> {
        self.connection
            .execute(
                "UPDATE providers SET obsolete_credential_origin = NULL WHERE id = ?1",
                [id],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    pub fn begin_provider_deletion(&mut self, id: ProviderId) -> Result<()> {
        let transaction = self.connection.transaction().map_err(sql_error)?;
        let changed = transaction
            .execute(
                "UPDATE providers SET deletion_pending = 1 WHERE id = ?1",
                [id],
            )
            .map_err(sql_error)?;
        if changed == 0 {
            return Err(not_found());
        }
        transaction
            .execute("UPDATE intelligence_settings SET active_provider = NULL WHERE active_provider = ?1", [id])
            .map_err(sql_error)?;
        transaction.commit().map_err(sql_error)
    }

    pub fn finish_provider_deletion(&mut self, id: ProviderId) -> Result<()> {
        let changed = self
            .connection
            .execute(
                "DELETE FROM providers WHERE id = ?1 AND deletion_pending = 1 AND obsolete_credential_origin IS NULL",
                [id],
            )
            .map_err(sql_error)?;
        if changed == 0 {
            return Err(not_found());
        }
        Ok(())
    }

    pub fn intelligence_settings(&self) -> Result<IntelligenceSettings> {
        self.connection
            .query_row(
                "SELECT active_provider FROM intelligence_settings WHERE id = 1",
                [],
                |row| {
                    Ok(IntelligenceSettings {
                        active_provider: row.get(0)?,
                    })
                },
            )
            .map_err(sql_error)
    }

    pub fn save_intelligence_settings(&mut self, settings: IntelligenceSettings) -> Result<()> {
        if let Some(provider) = settings.active_provider {
            self.provider(provider)?.require_available()?;
        }
        self.connection
            .execute(
                "UPDATE intelligence_settings SET active_provider = ?1 WHERE id = 1",
                [settings.active_provider],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    /// Admit a reservation only if the period's reserved and charged totals
    /// plus this one stay within every configured limit. The check and the
    /// insert share one immediate transaction, so concurrent callers cannot
    /// both pass on the same remaining budget.
    pub fn reserve_usage(
        &mut self,
        period: &str,
        reservation: Reservation,
        limits: Limits,
    ) -> Result<i64> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let (tokens, cost, unpriced): (i64, i64, i64) = transaction
            .query_row(
                "SELECT
                    coalesce(sum(CASE state WHEN 'reserved' THEN reserved_tokens ELSE charged_tokens END), 0),
                    coalesce(sum(CASE state WHEN 'reserved' THEN coalesce(reserved_cost, 0) ELSE coalesce(charged_cost, 0) END), 0),
                    coalesce(sum(CASE
                        WHEN state = 'reserved' AND reserved_cost IS NULL THEN reserved_tokens
                        WHEN state = 'settled' AND charged_cost IS NULL THEN charged_tokens
                        ELSE 0 END), 0)
                 FROM provider_usage WHERE provider_id = ?1 AND period = ?2",
                params![reservation.provider, period],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(sql_error)?;
        if let Some(limit) = limits.tokens
            && (tokens as u64).saturating_add(reservation.tokens) > limit
        {
            return Err(budget_exceeded(
                "This request could exceed the monthly token limit for the provider.",
            ));
        }
        if let Some(limit) = limits.cost {
            let Some(estimate) = reservation.cost else {
                return Err(budget_exceeded(
                    "A spending limit is set but the price is unknown, so the request was not sent.",
                ));
            };
            let earlier = match (unpriced, limits.unpriced_micros_per_million) {
                (0, _) => 0,
                (tokens, Some(price)) => cost_micros(tokens as u64, price),
                (_, None) => u64::MAX,
            };
            if (cost as u64)
                .saturating_add(earlier)
                .saturating_add(estimate)
                > limit
            {
                return Err(budget_exceeded(
                    "This request could exceed the monthly spending limit for the provider.",
                ));
            }
        }
        let tokens = i64::try_from(reservation.tokens).map_err(|_| storage_error())?;
        let cost = reservation
            .cost
            .map(i64::try_from)
            .transpose()
            .map_err(|_| storage_error())?;
        transaction
            .execute(
                "INSERT INTO provider_usage (provider_id, period, created_at, state, reserved_tokens, reserved_cost)
                 VALUES (?1, ?2, ?3, 'reserved', ?4, ?5)",
                params![reservation.provider, period, unix_ms()?, tokens, cost],
            )
            .map_err(sql_error)?;
        let id = transaction.last_insert_rowid();
        transaction.commit().map_err(sql_error)?;
        Ok(id)
    }

    pub fn settle_usage(&mut self, reservation: i64, settlement: Settlement) -> Result<()> {
        let to_i64 = |value: u64| i64::try_from(value).map_err(|_| storage_error());
        let changed = self
            .connection
            .execute(
                "UPDATE provider_usage SET state = 'settled', transmission = ?1, input_tokens = ?2,
                    output_tokens = ?3, usage_basis = ?4, charged_tokens = ?5, charged_cost = ?6,
                    message_id = ?7
                 WHERE id = ?8 AND state = 'reserved'",
                params![
                    settlement.transmission.as_str(),
                    settlement.input_tokens.map(to_i64).transpose()?,
                    settlement.output_tokens.map(to_i64).transpose()?,
                    settlement.basis.as_str(),
                    to_i64(settlement.charged_tokens)?,
                    settlement.charged_cost.map(to_i64).transpose()?,
                    settlement.message,
                    reservation,
                ],
            )
            .map_err(sql_error)?;
        if changed == 0 {
            return Err(InferenceError::new(
                ErrorCode::Storage,
                "The usage reservation was already settled.",
            ));
        }
        Ok(())
    }

    pub fn usage_summary(&self, provider: ProviderId, period: &str) -> Result<UsageSummary> {
        let row: (i64, i64, i64, i64, i64, i64) = self
            .connection
            .query_row(
                "SELECT count(*),
                    coalesce(sum(CASE WHEN state = 'settled' AND usage_basis = 'provider_reported' THEN charged_tokens END), 0),
                    coalesce(sum(CASE WHEN state = 'settled' AND usage_basis = 'estimated' THEN charged_tokens END), 0),
                    coalesce(sum(CASE WHEN state = 'reserved' THEN reserved_tokens END), 0),
                    coalesce(sum(CASE WHEN state = 'settled' THEN coalesce(charged_cost, 0)
                                      ELSE coalesce(reserved_cost, 0) END), 0),
                    coalesce(sum(CASE WHEN (state = 'settled' AND charged_cost IS NULL AND charged_tokens > 0)
                                        OR (state = 'reserved' AND reserved_cost IS NULL) THEN 1 END), 0)
                 FROM provider_usage WHERE provider_id = ?1 AND period = ?2",
                params![provider, period],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .map_err(sql_error)?;
        Ok(UsageSummary {
            requests: row.0 as u64,
            reported_tokens: row.1 as u64,
            estimated_tokens: row.2 as u64,
            reserved_tokens: row.3 as u64,
            cost_micros: row.4 as u64,
            unpriced_requests: row.5 as u64,
        })
    }
}

impl SqliteStore {
    /// Every text value in every table, concatenated. Leak checks use it to
    /// prove that secrets never reach the database.
    #[doc(hidden)]
    pub fn dump_text_for_tests(&self) -> String {
        let mut text = String::new();
        let Ok(mut tables) = self
            .connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        else {
            return text;
        };
        let names: Vec<String> = tables
            .query_map([], |row| row.get(0))
            .map(|rows| rows.filter_map(std::result::Result::ok).collect())
            .unwrap_or_default();
        for name in names {
            let query = format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""));
            let Ok(mut statement) = self.connection.prepare(&query) else {
                continue;
            };
            let columns = statement.column_count();
            let _ = statement
                .query_map([], |row| {
                    for index in 0..columns {
                        match row.get_ref(index)? {
                            rusqlite::types::ValueRef::Text(bytes)
                            | rusqlite::types::ValueRef::Blob(bytes) => {
                                text.push_str(&String::from_utf8_lossy(bytes));
                                text.push('\n');
                            }
                            _ => {}
                        }
                    }
                    Ok(())
                })
                .map(|rows| rows.for_each(drop));
        }
        text
    }
}

/// After a crash, a reservation's request may have been sent. Charge the
/// full reservation as an estimate with unknown transmission.
pub(crate) fn settle_interrupted(connection: &rusqlite::Connection) -> Result<()> {
    connection
        .execute(
            "UPDATE provider_usage SET state = 'settled', transmission = 'unknown',
                usage_basis = 'estimated', charged_tokens = reserved_tokens, charged_cost = reserved_cost
             WHERE state = 'reserved'",
            [],
        )
        .map_err(sql_error)?;
    Ok(())
}

#[cfg(test)]
#[path = "providers_tests.rs"]
mod tests;
