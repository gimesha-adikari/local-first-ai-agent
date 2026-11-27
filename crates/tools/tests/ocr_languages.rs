//! Opt-in multilingual OCR measurement through the production sandbox.
//! `AGENT_OCR_SAMPLES` holds synthetic PNGs with a `manifest.json` of ground
//! truth; `AGENT_OCR_TESSDATA` holds the language data to measure. Results
//! are printed as JSON lines and checked against the per-script thresholds
//! recorded in docs/stage-03-e-screen-intelligence.md.
#![cfg(target_os = "linux")]

use std::{collections::BTreeMap, path::PathBuf};

use agent_tools::ocr::{Tesseract, character_error_rate};

/// Highest accepted mean CER per script (synthetic 14, 16 dark and 20 px UI
/// text). Scripts without a threshold are reported but not gated.
/// Measured 2026-11-27 on an i5-8500 (synthetic, Noto fonts, Pango); the
/// limits sit just above the measured means so a regression fails.
const THRESHOLDS: &[(&str, f64)] = &[
    ("latin", 0.03),
    ("cyrillic", 0.03),
    ("arabic", 0.03),
    ("hebrew", 0.08),
    ("devanagari", 0.03),
    ("bengali", 0.03),
    ("hangul", 0.03),
    ("thai", 0.08),
    ("japanese", 0.10),
    ("han", 0.20),
    ("sinhala", 0.12),
    ("tamil", 0.12),
    ("mixed", 0.20),
];

#[test]
#[ignore = "needs synthetic samples and language data supplied through the environment"]
fn multilingual_recognition_meets_per_script_thresholds() {
    let samples = PathBuf::from(std::env::var_os("AGENT_OCR_SAMPLES").expect("sample directory"));
    let tessdata =
        PathBuf::from(std::env::var_os("AGENT_OCR_TESSDATA").expect("tessdata directory"));
    let tesseract = Tesseract::with_tessdata(&tessdata).expect("Tesseract with that data");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(samples.join("manifest.json")).unwrap()).unwrap();
    let mut per_script: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for sample in manifest.as_array().unwrap() {
        let languages: Vec<String> = sample["languages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|language| language.as_str().unwrap().to_owned())
            .collect();
        if !languages
            .iter()
            .all(|language| tesseract.languages().contains(language))
        {
            continue;
        }
        let png = std::fs::read(samples.join(sample["file"].as_str().unwrap())).unwrap();
        let output = tesseract.recognize(&png, &languages, &|| Ok(())).unwrap();
        let cer = character_error_rate(sample["truth"].as_str().unwrap(), &output.text);
        let script = sample["script"].as_str().unwrap().to_owned();
        println!(
            "{}",
            serde_json::json!({
                "file": sample["file"], "script": script, "languages": languages,
                "px": sample["px"], "dark": sample["dark"], "cer": (cer * 1000.0).round() / 1000.0,
                "ms": output.elapsed_ms, "text": output.text,
            })
        );
        per_script.entry(script).or_default().push(cer);
    }
    let mut failures = Vec::new();
    for (script, values) in &per_script {
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        println!(
            "{}",
            serde_json::json!({"summary": script, "mean_cer": (mean * 1000.0).round() / 1000.0, "n": values.len()})
        );
        if let Some((_, limit)) = THRESHOLDS.iter().find(|(name, _)| name == script)
            && mean > *limit
        {
            failures.push(format!("{script}: mean CER {mean:.3} above {limit}"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}
