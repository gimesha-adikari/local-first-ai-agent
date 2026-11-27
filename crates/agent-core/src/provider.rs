//! Provider configuration, sharing choices, pricing and budgets. Values
//! arrive from the user interface and are validated here before the engine
//! canonicalizes the endpoint and registers a destination. No credential
//! is ever part of these types.

use serde::{Deserialize, Serialize};

use crate::{
    capability::Retention,
    classification::{
        DataClass, HISTORY, LIBRARY, MEMORY, MODEL_OUTPUT, PDF_EXCERPT, PROVIDER_CONTROL,
        SYSTEM_TEXT, TEXT_EXCERPT, USER_INPUT,
    },
    inference::{ErrorCode, InferenceError, Result},
};

pub type ProviderId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderLocality {
    /// A third-party service on the internet.
    Cloud,
    /// A server the user runs: this computer, the local network or their own host.
    SelfHosted,
}

impl ProviderLocality {
    pub fn locality(self) -> crate::capability::Locality {
        match self {
            Self::Cloud => crate::capability::Locality::ThirdParty,
            Self::SelfHosted => crate::capability::Locality::SelfHosted,
        }
    }
}

/// Provider-side retention as disclosed to the user. A provider with an
/// unknown retention cannot receive personal data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRetention {
    /// Not retained, for example a self-hosted server without logging or a
    /// zero-retention agreement.
    None,
    /// Retained according to the provider's policy, summarized by the user.
    ProviderPolicy,
    Unknown,
}

impl ProviderRetention {
    pub fn retention(self) -> Retention {
        match self {
            Self::None => Retention::None,
            Self::ProviderPolicy => Retention::ProviderDeclared,
            Self::Unknown => Retention::Unknown,
        }
    }
}

/// Which categories of local information may be sent to this provider.
/// The current message always goes to the selected provider; everything
/// else is withheld unless enabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Sharing {
    pub history: bool,
    pub memories: bool,
    pub documents: bool,
}

impl Sharing {
    /// Classes a payload for this provider may carry. Anything else is
    /// withheld before the request is built and refused by the consent check.
    pub fn allowed_classes(self) -> Vec<DataClass> {
        let mut classes = vec![USER_INPUT, SYSTEM_TEXT, PROVIDER_CONTROL];
        if self.history {
            classes.extend([HISTORY, MODEL_OUTPUT]);
        }
        if self.memories {
            classes.push(MEMORY);
        }
        if self.documents {
            classes.extend([LIBRARY, PDF_EXCERPT, TEXT_EXCERPT]);
        }
        classes
    }
}

/// Prices the user entered from their provider's price list, in millionths
/// of the currency unit per million tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pricing {
    pub currency: String,
    pub input_micros_per_million: u64,
    pub output_micros_per_million: u64,
}

/// Monthly limits, enforced on reservations before each request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Budget {
    pub monthly_tokens: Option<u64>,
    /// Requires pricing; without it no request can be admitted.
    pub monthly_cost_micros: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderSettings {
    pub name: String,
    pub endpoint: String,
    pub locality: ProviderLocality,
    pub model: String,
    pub context_tokens: u32,
    pub max_output_tokens: u32,
    pub temperature: f32,
    /// The endpoint accepts image parts for this model.
    pub image_input: bool,
    /// Ask for usage in the stream. Some compatible servers reject it.
    pub stream_usage: bool,
    pub requires_key: bool,
    pub retention: ProviderRetention,
    pub retention_summary: String,
    pub sharing: Sharing,
    pub pricing: Option<Pricing>,
    pub budget: Budget,
}

pub const MAX_PROVIDERS: usize = 16;
pub const MIN_CONTEXT_TOKENS: u32 = 1024;
pub const MAX_CONTEXT_TOKENS: u32 = 2_000_000;
pub const MAX_OUTPUT_TOKENS: u32 = 32_768;
const MAX_PRICE_MICROS: u64 = 1_000_000_000_000;
const MAX_BUDGET_TOKENS: u64 = 1_000_000_000_000;

fn invalid(message: &str) -> InferenceError {
    InferenceError::new(ErrorCode::InvalidConfig, message)
}

fn plain(text: &str, max: usize) -> bool {
    let text = text.trim();
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

impl ProviderSettings {
    pub fn validate(&self) -> Result<()> {
        if !plain(&self.name, 64) {
            return Err(invalid("Enter a provider name of 1–64 characters."));
        }
        if !plain(&self.endpoint, 512) {
            return Err(invalid("Enter the provider's API URL."));
        }
        if !plain(&self.model, 200) || self.model.chars().any(char::is_whitespace) {
            return Err(invalid(
                "Enter a model identifier of 1–200 characters without spaces.",
            ));
        }
        if !(MIN_CONTEXT_TOKENS..=MAX_CONTEXT_TOKENS).contains(&self.context_tokens) {
            return Err(invalid(
                "Context must be between 1,024 and 2,000,000 tokens.",
            ));
        }
        if self.max_output_tokens == 0
            || self.max_output_tokens > MAX_OUTPUT_TOKENS
            || self.max_output_tokens >= self.context_tokens
        {
            return Err(invalid(
                "Use 1–32,768 output tokens, fewer than the context size.",
            ));
        }
        if !(0.0..=2.0).contains(&self.temperature) || !self.temperature.is_finite() {
            return Err(invalid("Temperature must be between 0 and 2."));
        }
        if self.retention_summary.len() > 500
            || self
                .retention_summary
                .chars()
                .any(|c| c.is_control() && c != '\n')
        {
            return Err(invalid("Keep the retention summary under 500 characters."));
        }
        if self.retention == ProviderRetention::ProviderPolicy
            && self.retention_summary.trim().is_empty()
        {
            return Err(invalid(
                "Summarize how long the provider keeps requests, from its data policy.",
            ));
        }
        if self.locality == ProviderLocality::Cloud && !self.requires_key {
            return Err(invalid("Cloud providers need an API key."));
        }
        if let Some(pricing) = &self.pricing {
            if pricing.currency.len() != 3
                || !pricing.currency.bytes().all(|b| b.is_ascii_uppercase())
            {
                return Err(invalid("Use a three-letter currency code such as USD."));
            }
            if pricing.input_micros_per_million > MAX_PRICE_MICROS
                || pricing.output_micros_per_million > MAX_PRICE_MICROS
            {
                return Err(invalid("The configured price is too large."));
            }
        }
        if self
            .budget
            .monthly_tokens
            .is_some_and(|tokens| tokens == 0 || tokens > MAX_BUDGET_TOKENS)
            || self
                .budget
                .monthly_cost_micros
                .is_some_and(|cost| cost == 0 || cost > MAX_PRICE_MICROS)
        {
            return Err(invalid("Budget limits must be positive."));
        }
        if self.budget.monthly_cost_micros.is_some() && self.pricing.is_none() {
            return Err(invalid("A spending limit needs prices for this model."));
        }
        Ok(())
    }
}

/// Which intelligence answers Chat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntelligenceSettings {
    /// The provider Chat uses. `None` means the local model.
    pub active_provider: Option<ProviderId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn sample() -> ProviderSettings {
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
            retention_summary: "Kept 30 days for abuse monitoring.".into(),
            sharing: Sharing {
                history: true,
                memories: false,
                documents: false,
            },
            pricing: None,
            budget: Budget {
                monthly_tokens: Some(100_000),
                monthly_cost_micros: None,
            },
        }
    }

    #[test]
    fn validates_settings_and_budget_requirements() {
        assert!(sample().validate().is_ok());
        let mut settings = sample();
        settings.budget.monthly_cost_micros = Some(5_000_000);
        assert!(
            settings.validate().is_err(),
            "a spending limit needs prices"
        );
        settings.pricing = Some(Pricing {
            currency: "USD".into(),
            input_micros_per_million: 2_500_000,
            output_micros_per_million: 10_000_000,
        });
        assert!(settings.validate().is_ok());
        for broken in [
            ProviderSettings {
                model: "two words".into(),
                ..sample()
            },
            ProviderSettings {
                max_output_tokens: 8192,
                ..sample()
            },
            ProviderSettings {
                temperature: f32::NAN,
                ..sample()
            },
            ProviderSettings {
                retention_summary: String::new(),
                ..sample()
            },
            ProviderSettings {
                requires_key: false,
                ..sample()
            },
            ProviderSettings {
                name: " ".into(),
                ..sample()
            },
        ] {
            assert!(broken.validate().is_err(), "{broken:?}");
        }
        assert!(
            ProviderSettings {
                requires_key: false,
                locality: ProviderLocality::SelfHosted,
                retention: ProviderRetention::None,
                retention_summary: String::new(),
                ..sample()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn sharing_controls_the_allowed_classes() {
        let none = Sharing::default().allowed_classes();
        assert!(none.contains(&USER_INPUT));
        assert!(!none.contains(&HISTORY) && !none.contains(&MEMORY) && !none.contains(&LIBRARY));
        let all = Sharing {
            history: true,
            memories: true,
            documents: true,
        }
        .allowed_classes();
        for class in [
            HISTORY,
            MODEL_OUTPUT,
            MEMORY,
            LIBRARY,
            PDF_EXCERPT,
            TEXT_EXCERPT,
        ] {
            assert!(all.contains(&class));
        }
        assert_eq!(IntelligenceSettings::default().active_provider, None);
    }
}
