//! Attachment sharing with a remote provider through the real boundary,
//! against the loopback fixture. Synthetic content only.

use agent_core::{
    attachment::AttachmentRetention, inference::ErrorCode, provider::IntelligenceSettings,
};
use serde_json::json;

use super::{Remote, remote, streaming};
use crate::tests::attachment_tests::{Synthetic, attach_ready};

fn ask(
    r: &Remote,
    id: i64,
    attachments: serde_json::Value,
) -> agent_core::inference::Result<agent_core::chat::Message> {
    r.send(
        id,
        "What does the attachment say?",
        json!({ "attachments": attachments }),
    )
}

#[test]
fn attached_text_reaches_a_provider_only_with_a_matching_per_request_authorization() {
    let r = remote(streaming(true), |_| {});
    let id = r.conversation();
    let attachment = attach_ready(
        &r.engine,
        Some(id),
        AttachmentRetention::Temporary,
        Synthetic::text("plan.md", "The CORMORANT launch is in May."),
    );
    // Selecting a file never authorizes sending it.
    let error = ask(&r, id, json!({"ids": [attachment]})).unwrap_err();
    assert_eq!(error.code, ErrorCode::PolicyDenied);
    // An authorization for another provider or other attachments is refused.
    let wrong = ask(
        &r,
        id,
        json!({"ids": [attachment], "share": {"provider": r.provider + 1, "ids": [attachment]}}),
    );
    assert_eq!(wrong.unwrap_err().code, ErrorCode::PolicyDenied);
    let wrong = ask(
        &r,
        id,
        json!({"ids": [attachment], "share": {"provider": r.provider, "ids": []}}),
    );
    assert_eq!(wrong.unwrap_err().code, ErrorCode::PolicyDenied);
    assert_eq!(r.server.accepted(), 0);
    // Image bytes need a provider that accepts images.
    let photo = attach_ready(
        &r.engine,
        Some(id),
        AttachmentRetention::Temporary,
        Synthetic::image("photo.png", Some("SIGN TEXT")),
    );
    let error = ask(
        &r,
        id,
        json!({"ids": [photo], "share": {"provider": r.provider, "ids": [photo], "images": [photo]}}),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedCapability);
    assert_eq!(r.server.accepted(), 0);

    let message = ask(
        &r,
        id,
        json!({"ids": [attachment], "share": {"provider": r.provider, "ids": [attachment]}}),
    )
    .unwrap();
    assert_eq!(message.content, "Hi there");
    let body = r.request_bodies().pop().unwrap().to_string();
    assert!(body.contains("CORMORANT") && body.contains("plan.md"));
    assert!(!body.contains("image_url"));
    let receipt = r.receipt(message.id);
    let labels = receipt["external"][0]["labels"].to_string();
    assert!(labels.contains("core.attachment-text/1") && labels.contains("core.attachment/1"));
    assert!(receipt["used"].to_string().contains("\"attachment\""));
    let record = message.context.unwrap().attachments.unwrap();
    assert!(record.shared_remotely);
    // Nothing attached was written to the database.
    assert!(!r.database_text().contains("CORMORANT"));

    // Recognized text only: the image itself is not sent.
    ask(
        &r,
        id,
        json!({"ids": [photo], "share": {"provider": r.provider, "ids": [photo]}}),
    )
    .unwrap();
    let body = r.request_bodies().pop().unwrap().to_string();
    assert!(body.contains("SIGN TEXT") && !body.contains("image_url"));

    // Selecting the local model never falls back to the configured provider.
    r.engine
        .set_intelligence_settings(IntelligenceSettings {
            active_provider: None,
        })
        .unwrap();
    let before = r.server.accepted();
    let error = ask(
        &r,
        id,
        json!({"ids": [attachment], "share": {"provider": r.provider, "ids": [attachment]}}),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::NoModel);
    assert_eq!(r.server.accepted(), before);
}

#[test]
fn a_chosen_image_is_sent_validated_to_an_image_provider() {
    let r = remote(streaming(true), |settings| settings.image_input = true);
    let id = r.conversation();
    let photo = attach_ready(
        &r.engine,
        Some(id),
        AttachmentRetention::Temporary,
        Synthetic::image("photo.png", None),
    );
    let message = ask(
        &r,
        id,
        json!({"ids": [photo], "share": {"provider": r.provider, "ids": [photo], "images": [photo]}}),
    )
    .unwrap();
    let body = r.request_bodies().pop().unwrap();
    let parts = body["messages"].as_array().unwrap().last().unwrap()["content"].clone();
    let images: Vec<_> = parts
        .as_array()
        .unwrap()
        .iter()
        .filter(|part| part["type"] == "image_url")
        .collect();
    assert_eq!(images.len(), 1);
    let expected = crate::providers::openai::base64(b"\x89PNG\r\n\x1a\nsynthetic-image");
    assert_eq!(
        images[0]["image_url"]["url"],
        format!("data:image/png;base64,{expected}")
    );
    let record = message.context.unwrap().attachments.unwrap();
    assert!(record.attachments[0].image_sent);
    let receipt = r.receipt(message.id);
    assert!(receipt["used"].to_string().contains("attachment_image"));
    assert!(
        receipt["external"][0]["labels"]
            .to_string()
            .contains("core.attachment-image/1")
    );
}
