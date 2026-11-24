use agent_core::inference::{ErrorCode, GenerationRequest, ModelConfig};

#[test]
fn validates_defaults_and_rejects_invalid_limits() {
    assert!(ModelConfig::default().validate().is_ok());
    assert_eq!(
        ModelConfig {
            threads: 0,
            ..ModelConfig::default()
        }
        .validate()
        .unwrap_err()
        .code,
        ErrorCode::InvalidConfig
    );
    assert!(
        ModelConfig {
            context_length: 32,
            ..ModelConfig::default()
        }
        .validate()
        .is_err()
    );
    let mut request = GenerationRequest::new("Hello");
    assert!(request.validate().is_ok());
    request.temperature = f32::NAN;
    assert!(request.validate().is_err());
}

#[test]
fn rejects_blank_prompts_and_unbounded_generation() {
    assert!(GenerationRequest::new("   ").validate().is_err());
    let mut request = GenerationRequest::new("Hello");
    request.max_tokens = 0;
    assert!(request.validate().is_err());
    request.max_tokens = 32;
    request.top_p = 1.1;
    assert!(request.validate().is_err());
}
