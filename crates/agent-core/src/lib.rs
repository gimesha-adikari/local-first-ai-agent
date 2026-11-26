//! Shared application logic for local-first-ai-agent.

/// Returns a message confirming that the shared core was reached.
pub fn status() -> &'static str {
    "Agent core is ready."
}

pub mod inference;
pub mod intelligence;

pub mod capability;
pub mod chat;
pub mod chat_context;
pub mod chat_route;
pub mod classification;

pub mod agent;

pub mod knowledge;

pub mod provider;

pub mod semantic;
