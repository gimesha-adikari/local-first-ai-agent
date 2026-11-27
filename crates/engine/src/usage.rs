//! Usage estimates, prices and budget periods. Estimates are deliberately
//! high so a reservation rarely undercounts; provider reports replace them
//! when they arrive.

use std::time::{SystemTime, UNIX_EPOCH};

use agent_core::{chat::PromptMessage, provider::Pricing};
pub use storage::providers::cost_micros;

/// The calendar month in UTC, as `YYYY-MM`.
pub fn period(now: SystemTime) -> String {
    let days = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() / 86_400) as i64;
    // Civil-from-days (Howard Hinnant), valid for the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}")
}

pub fn current_period() -> String {
    period(SystemTime::now())
}

/// A conservative token estimate without the provider's tokenizer: one
/// token per two UTF-8 bytes, which overestimates English text and stays
/// close for scripts that need more bytes per token.
pub fn estimate_text(text: &str) -> u64 {
    (text.len() as u64).div_ceil(2)
}

/// A conservative estimate for one image: 170 tokens per 512-pixel tile
/// plus 85, after limiting the longer side to 2048 pixels. Providers that
/// scale images down charge less; none documented here charge more.
pub fn estimate_image(width: u32, height: u32) -> u64 {
    let longest = width.max(height).max(1);
    let scale = (2048.0 / f64::from(longest)).min(1.0);
    let tiles = |side: u32| ((f64::from(side) * scale) / 512.0).ceil().max(1.0) as u64;
    85 + 170 * tiles(width) * tiles(height)
}

/// Estimated prompt tokens, including per-message framing.
pub fn estimate_messages(messages: &[PromptMessage<'_>]) -> u64 {
    messages
        .iter()
        .map(|message| estimate_text(message.content) + 8)
        .sum::<u64>()
        + 16
}

/// Estimated cost of a request reserving `input` and `output` tokens.
pub fn estimate_cost(pricing: Option<&Pricing>, input: u64, output: u64) -> Option<u64> {
    let pricing = pricing?;
    Some(
        cost_micros(input, pricing.input_micros_per_million)
            .saturating_add(cost_micros(output, pricing.output_micros_per_million)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn periods_follow_the_utc_calendar() {
        let at = |seconds: u64| period(UNIX_EPOCH + Duration::from_secs(seconds));
        assert_eq!(at(0), "1970-01");
        assert_eq!(at(951_782_400), "2000-02"); // 2000-02-29
        assert_eq!(at(951_868_799), "2000-02");
        assert_eq!(at(951_868_800), "2000-03");
        assert_eq!(at(1_798_761_599), "2026-12"); // 2026-12-31 23:59:59
        assert_eq!(at(1_798_761_600), "2027-01");
    }

    #[test]
    fn estimates_are_conservative() {
        assert_eq!(estimate_text("abcd"), 2);
        assert_eq!(estimate_text("日本"), 3);
        let pricing = Pricing {
            currency: "USD".into(),
            input_micros_per_million: 3_000_000,
            output_micros_per_million: 15_000_000,
        };
        assert_eq!(
            estimate_cost(Some(&pricing), 1_000_000, 100_000),
            Some(4_500_000)
        );
        assert_eq!(estimate_cost(None, 1, 1), None);
    }
}
