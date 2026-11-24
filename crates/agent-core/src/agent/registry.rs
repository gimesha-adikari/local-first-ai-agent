use std::collections::BTreeMap;

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json, value::RawValue};

use super::{
    AgentError, AgentErrorCode, AgentResult, ApplicationArgs, CalculatorArgs, FileReadArgs,
    FileSearchArgs, MAX_OBSERVATION_BYTES, MAX_PROPOSAL_BYTES, PermissionRequirement, Risk,
    ToolArguments, ToolDefinition, ToolId, ValidatedCall,
    tool::{valid_filter, valid_identifier, valid_relative_path, valid_text},
};

#[derive(Debug, Default)]
pub struct ToolRegistry {
    definitions: BTreeMap<ToolId, ToolDefinition>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProposalKind {
    ToolCall,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal<'a> {
    #[serde(rename = "kind")]
    _kind: ProposalKind,
    tool: String,
    #[serde(borrow)]
    arguments: &'a RawValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Explicitly register the six fixed MVP contracts. Adapters are not
    /// registered here; the runtime must expose only supported capabilities.
    pub fn mvp() -> AgentResult<Self> {
        let mut registry = Self::new();
        for id in ToolId::ALL {
            registry.register(id)?;
        }
        Ok(registry)
    }

    pub fn register(&mut self, id: ToolId) -> AgentResult<()> {
        if self.definitions.contains_key(&id) {
            return Err(AgentError::new(
                AgentErrorCode::InvalidArguments,
                "The tool identifier is already registered.",
            ));
        }
        self.definitions.insert(id, definition(id));
        Ok(())
    }

    pub fn get(&self, id: ToolId) -> Option<&ToolDefinition> {
        self.definitions.get(&id)
    }

    pub fn definitions(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.definitions.values()
    }

    /// Accept exactly one complete JSON proposal, not fenced code, prose,
    /// partial streaming output, or reasoning. Retain raw argument JSON so
    /// typed deserialization can reject duplicate fields rather than silently
    /// accepting a last value from an intermediate JSON object.
    pub fn parse(&self, source: &str) -> AgentResult<ValidatedCall> {
        if source.len() > MAX_PROPOSAL_BYTES {
            return Err(invalid_proposal());
        }
        let proposal: Proposal<'_> = serde_json::from_str(source).map_err(|_| invalid_proposal())?;
        let id = self
            .definitions
            .keys()
            .copied()
            .find(|id| id.as_str() == proposal.tool)
            .ok_or_else(unknown_tool)?;
        self.validate(id, proposal.arguments.get())
    }

    /// Also usable by a future model-specific adapter with raw argument JSON.
    pub fn validate(&self, id: ToolId, arguments: &str) -> AgentResult<ValidatedCall> {
        if !self.definitions.contains_key(&id) {
            return Err(unknown_tool());
        }
        if arguments.len() > MAX_PROPOSAL_BYTES || !arguments.trim_start().starts_with('{') {
            return Err(invalid_arguments());
        }
        let args = match id {
            ToolId::Calculate => {
                let args: CalculatorArgs = decode(arguments)?;
                if !valid_text(&args.expression, 256) {
                    return Err(invalid_arguments());
                }
                ToolArguments::Calculate(args)
            }
            ToolId::DeviceInfo | ToolId::AppsList => {
                let _: EmptyArgs = decode(arguments)?;
                ToolArguments::Empty {}
            }
            ToolId::FileSearch => {
                let args: FileSearchArgs = decode(arguments)?;
                if !valid_identifier(&args.scope_id)
                    || args.name_contains.as_deref().is_some_and(|s| !valid_filter(s, 128))
                    || args.extension.as_deref().is_some_and(|s| {
                        s.is_empty() || s.len() > 32 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    })
                    || !(1..=100).contains(&args.max_results)
                    || args.max_depth > 8
                {
                    return Err(invalid_arguments());
                }
                ToolArguments::FileSearch(args)
            }
            ToolId::FileRead => {
                let args: FileReadArgs = decode(arguments)?;
                if !valid_identifier(&args.scope_id)
                    || args.relative_path.as_deref().is_some_and(|s| !valid_relative_path(s))
                {
                    return Err(invalid_arguments());
                }
                ToolArguments::FileRead(args)
            }
            ToolId::AppLaunch => {
                let args: ApplicationArgs = decode(arguments)?;
                if !valid_identifier(&args.app_id) {
                    return Err(invalid_arguments());
                }
                ToolArguments::AppLaunch(args)
            }
        };
        Ok(ValidatedCall::new(id, args))
    }
}

fn decode<T: DeserializeOwned>(source: &str) -> AgentResult<T> {
    serde_json::from_str(source).map_err(|_| invalid_arguments())
}

fn invalid_proposal() -> AgentError {
    AgentError::new(AgentErrorCode::InvalidProposal, "Expected one complete tool proposal.")
}

fn invalid_arguments() -> AgentError {
    AgentError::new(AgentErrorCode::InvalidArguments, "Tool arguments do not match the registered contract.")
}

fn unknown_tool() -> AgentError {
    AgentError::new(AgentErrorCode::UnknownTool, "The requested tool is not registered.")
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn text(max: usize) -> Value {
    json!({"type": "string", "minLength": 1, "maxLength": max})
}

fn identifier() -> Value {
    json!({"type": "string", "minLength": 1, "maxLength": 64, "pattern": "^[A-Za-z0-9_-]+$"})
}

fn nullable(schema: Value) -> Value {
    json!({"anyOf": [schema, {"type": "null"}]})
}

fn unsigned() -> Value {
    json!({"type": "integer", "minimum": 0})
}

fn definition(id: ToolId) -> ToolDefinition {
    let empty = || object(json!({}), &[]);
    let (description, input_schema, output_schema, risk, permission, timeout_ms, side_effect) =
        match id {
            ToolId::Calculate => (
                "Evaluate a bounded arithmetic expression.",
                object(json!({"expression": text(256)}), &["expression"]),
                object(json!({"value": {"type": "number"}}), &["value"]),
                Risk::Low,
                PermissionRequirement::Automatic,
                1000,
                "None",
            ),
            ToolId::DeviceInfo => (
                "Read basic OS, CPU, memory, storage and optional GPU information.",
                empty(),
                object(json!({
                    "os": text(256), "architecture": text(64), "cpu": text(256),
                    "logical_cpus": {"type":"integer","minimum":1},
                    "memory_total_bytes": unsigned(),
                    "memory_available_bytes": nullable(unsigned()),
                    "storage_total_bytes": nullable(unsigned()),
                    "storage_available_bytes": nullable(unsigned()),
                    "gpu": nullable(text(256))
                }), &["os","architecture","cpu","logical_cpus","memory_total_bytes","memory_available_bytes","storage_total_bytes","storage_available_bytes","gpu"]),
                Risk::Low,
                PermissionRequirement::Automatic,
                3000,
                "None",
            ),
            ToolId::FileSearch => (
                "Search filenames in an authorized directory; extension excludes the leading dot. Depth zero searches immediate entries.",
                object(json!({
                    "scope_id": identifier(),
                    "name_contains": nullable(text(128)),
                    "extension": nullable(json!({"type":"string","pattern":"^[A-Za-z0-9_-]{1,32}$"})),
                    "max_results": {"type":"integer","minimum":1,"maximum":100,"default":100},
                    "max_depth": {"type":"integer","minimum":0,"maximum":8,"default":8}
                }), &["scope_id"]),
                object(json!({"paths":{"type":"array","maxItems":100,"items":text(1024)},"truncated":{"type":"boolean"}}), &["paths","truncated"]),
                Risk::ScopedRead,
                PermissionRequirement::DirectoryRead,
                5000,
                "Reads authorized filenames only",
            ),
            ToolId::FileRead => (
                "Read a bounded UTF-8 text preview. Omit relative_path for a selected file; use a relative path for an authorized directory.",
                object(json!({"scope_id":identifier(),"relative_path":nullable(text(1024))}), &["scope_id"]),
                object(json!({"content":{"type":"string","maxLength":4096},"file_bytes":{"type":"integer","minimum":0,"maximum":65536},"truncated":{"type":"boolean"}}), &["content","file_bytes","truncated"]),
                Risk::ScopedRead,
                PermissionRequirement::ResourceRead,
                3000,
                "Reads authorized text only",
            ),
            ToolId::AppsList => (
                "List discoverable local desktop applications and their opaque identifiers.",
                empty(),
                object(json!({"applications":{"type":"array","maxItems":100,"items":object(json!({"app_id":identifier(),"display_name":text(256)}), &["app_id","display_name"])},"truncated":{"type":"boolean"}}), &["applications","truncated"]),
                Risk::Low,
                PermissionRequirement::Automatic,
                5000,
                "None",
            ),
            ToolId::AppLaunch => (
                "Request launch of an application from trusted discovery. Requires approval for every launch.",
                object(json!({"app_id":identifier()}), &["app_id"]),
                object(json!({"app_id":identifier(),"dispatched":{"const":true}}), &["app_id","dispatched"]),
                Risk::SideEffect,
                PermissionRequirement::ConfirmEveryLaunch,
                5000,
                "May start an external application; it can have its own network access",
            ),
        };
    ToolDefinition {
        id,
        description,
        input_schema,
        output_schema,
        risk,
        permission,
        timeout_ms,
        max_observation_bytes: MAX_OBSERVATION_BYTES,
        side_effect,
    }
}
