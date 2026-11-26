//! Fixed MVP tool contracts. A validated request is not an authorization.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AgentError, AgentErrorCode, AgentResult};

pub const MAX_PROPOSAL_BYTES: usize = 16 * 1024;
pub const MAX_OBSERVATION_BYTES: usize = 8 * 1024;
pub const MAX_PDF_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_PDF_PAGES: u32 = 256;
pub const MAX_PDF_RANGE_PAGES: u32 = 4;
pub const MAX_PDF_TEXT_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum ToolId {
    #[serde(rename = "math.calculate")]
    Calculate,
    #[serde(rename = "device.info")]
    DeviceInfo,
    #[serde(rename = "files.search")]
    FileSearch,
    #[serde(rename = "files.read")]
    FileRead,
    #[serde(rename = "apps.list")]
    AppsList,
    #[serde(rename = "apps.launch")]
    AppLaunch,
}

impl ToolId {
    pub const ALL: [Self; 6] = [
        Self::Calculate,
        Self::DeviceInfo,
        Self::FileSearch,
        Self::FileRead,
        Self::AppsList,
        Self::AppLaunch,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Calculate => "math.calculate",
            Self::DeviceInfo => "device.info",
            Self::FileSearch => "files.search",
            Self::FileRead => "files.read",
            Self::AppsList => "apps.list",
            Self::AppLaunch => "apps.launch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    ScopedRead,
    SideEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRequirement {
    Automatic,
    DirectoryRead,
    ResourceRead,
    ConfirmEveryLaunch,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub id: ToolId,
    pub description: &'static str,
    pub input_schema: Value,
    pub output_schema: Value,
    pub risk: Risk,
    pub permission: PermissionRequirement,
    pub timeout_ms: u64,
    pub max_observation_bytes: usize,
    pub side_effect: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalculatorArgs {
    pub expression: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileSearchArgs {
    pub scope_id: String,
    pub name_contains: Option<String>,
    pub extension: Option<String>,
    #[serde(default = "default_results")]
    pub max_results: u32,
    #[serde(default = "default_depth")]
    pub max_depth: u32,
}

fn default_results() -> u32 {
    100
}

fn default_depth() -> u32 {
    8
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileReadArgs {
    pub scope_id: String,
    /// None refers to a selected file, not a directory listing.
    pub relative_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_pages: Option<PdfPageRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PdfPageRange {
    pub first_page: u32,
    pub last_page: u32,
}

impl PdfPageRange {
    pub(super) fn is_valid(self) -> bool {
        self.first_page > 0
            && self.last_page <= MAX_PDF_PAGES
            && self.last_page >= self.first_page
            && self.last_page - self.first_page < MAX_PDF_RANGE_PAGES
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PdfPageText {
    pub page_number: u32,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationArgs {
    /// An opaque identifier supplied by trusted discovery, not a command.
    pub app_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ToolArguments {
    Calculate(CalculatorArgs),
    FileSearch(FileSearchArgs),
    FileRead(FileReadArgs),
    AppLaunch(ApplicationArgs),
    Empty {},
}

/// Only the registry may construct this value after validation. The policy
/// engine must still independently grant access before dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidatedCall {
    tool: ToolId,
    arguments: ToolArguments,
}

impl ValidatedCall {
    pub(super) fn new(tool: ToolId, arguments: ToolArguments) -> Self {
        Self { tool, arguments }
    }

    pub const fn tool(&self) -> ToolId {
        self.tool
    }

    pub fn arguments(&self) -> &ToolArguments {
        &self.arguments
    }

    pub fn scope_id(&self) -> Option<&str> {
        match &self.arguments {
            ToolArguments::FileSearch(args) => Some(&args.scope_id),
            ToolArguments::FileRead(args) => Some(&args.scope_id),
            _ => None,
        }
    }
}

pub(super) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(super) fn valid_text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

/// A portable lexical restriction only. Native adapters must enforce
/// descriptor-relative access, resource type and race-resistant containment.
pub(super) fn valid_relative_path(value: &str) -> bool {
    valid_text(value, 1024)
        && !value.contains(['\\', ':'])
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

pub(super) fn valid_filter(value: &str, limit: usize) -> bool {
    valid_text(value, limit) && !value.contains(['/', '\\', ':'])
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub os: String,
    pub architecture: String,
    pub cpu: String,
    pub logical_cpus: u32,
    pub memory_total_bytes: u64,
    pub memory_available_bytes: Option<u64>,
    pub storage_total_bytes: Option<u64>,
    pub storage_available_bytes: Option<u64>,
    pub gpu: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppEntry {
    pub app_id: String,
    pub display_name: String,
}

/// Adapter results contain data, not executable instructions. Launch means
/// dispatch accepted, never that the application completed any further task.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ToolOutput {
    Calculate {
        value: f64,
    },
    DeviceInfo(DeviceInfo),
    FileSearch {
        paths: Vec<String>,
        truncated: bool,
        extension: Option<String>,
    },
    FileRead {
        content: String,
        file_bytes: u64,
        truncated: bool,
    },
    PdfRead {
        pages: Vec<PdfPageText>,
        page_count: u32,
        first_page: u32,
        last_page: u32,
        file_bytes: u64,
        truncated: bool,
    },
    AppsList {
        applications: Vec<AppEntry>,
        truncated: bool,
    },
    AppLaunch {
        app_id: String,
        dispatched: bool,
    },
}

impl ToolOutput {
    fn valid_for(&self, call: &ValidatedCall) -> bool {
        match (self, call.tool(), call.arguments()) {
            (Self::Calculate { value }, ToolId::Calculate, ToolArguments::Calculate(_)) => {
                value.is_finite()
            }
            (Self::DeviceInfo(info), ToolId::DeviceInfo, ToolArguments::Empty {}) => {
                valid_text(&info.os, 256)
                    && valid_text(&info.architecture, 64)
                    && valid_text(&info.cpu, 256)
                    && info.logical_cpus > 0
                    && info
                        .memory_available_bytes
                        .is_none_or(|n| n <= info.memory_total_bytes)
                    && info
                        .storage_available_bytes
                        .zip(info.storage_total_bytes)
                        .is_none_or(|(a, t)| a <= t)
                    && info.gpu.as_deref().is_none_or(|gpu| valid_text(gpu, 256))
            }
            (
                Self::FileSearch {
                    paths, extension, ..
                },
                ToolId::FileSearch,
                ToolArguments::FileSearch(args),
            ) => {
                *extension == args.extension.as_deref().map(str::to_ascii_lowercase)
                    && paths.len() <= args.max_results as usize
                    && paths.iter().all(|path| {
                        valid_relative_path(path)
                            && extension.as_deref().is_none_or(|extension| {
                                std::path::Path::new(path)
                                    .extension()
                                    .and_then(|suffix| suffix.to_str())
                                    .is_some_and(|suffix| suffix.eq_ignore_ascii_case(extension))
                            })
                    })
            }
            (
                Self::FileRead {
                    content,
                    file_bytes,
                    truncated,
                },
                ToolId::FileRead,
                ToolArguments::FileRead(args),
            ) => {
                args.pdf_pages.is_none()
                    && *file_bytes <= 64 * 1024
                    && content.len() <= 4096
                    && !content.contains('\0')
                    && content.len() as u64 <= *file_bytes
                    && (*truncated == ((content.len() as u64) < *file_bytes))
            }
            (
                Self::PdfRead {
                    pages,
                    page_count,
                    first_page,
                    last_page,
                    file_bytes,
                    truncated,
                },
                ToolId::FileRead,
                ToolArguments::FileRead(args),
            ) => {
                let range = PdfPageRange {
                    first_page: *first_page,
                    last_page: *last_page,
                };
                range.is_valid()
                    && args.pdf_pages.map_or_else(
                        || *first_page == 1 && *last_page == (*page_count).min(MAX_PDF_RANGE_PAGES),
                        |requested| requested == range,
                    )
                    && (*last_page..=MAX_PDF_PAGES).contains(page_count)
                    && (1..=MAX_PDF_BYTES).contains(file_bytes)
                    && pages.len() == (*last_page - *first_page + 1) as usize
                    && (*first_page..=*last_page).zip(pages).all(|(number, page)| {
                        page.page_number == number && !page.text.contains('\0')
                    })
                    && pages.iter().map(|page| page.text.len()).sum::<usize>() <= MAX_PDF_TEXT_BYTES
                    && (*truncated
                        == (*first_page != 1
                            || last_page != page_count
                            || pages.iter().any(|page| page.truncated)))
            }
            (Self::AppsList { applications, .. }, ToolId::AppsList, ToolArguments::Empty {}) => {
                applications.len() <= 100
                    && applications.iter().all(|app| {
                        valid_identifier(&app.app_id) && valid_text(&app.display_name, 256)
                    })
            }
            (
                Self::AppLaunch { app_id, dispatched },
                ToolId::AppLaunch,
                ToolArguments::AppLaunch(args),
            ) => *dispatched && app_id == &args.app_id,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolOutcome {
    Success { result: ToolOutput },
    Failure { error: AgentError },
}

/// Construct observations only from adapter results, never model claims.
/// The entire serialized envelope, including escaping, is size checked.
#[derive(Debug, Clone, Serialize)]
pub struct ToolObservation {
    tool: ToolId,
    #[serde(flatten)]
    outcome: ToolOutcome,
}

impl ToolObservation {
    pub fn success(call: &ValidatedCall, result: ToolOutput) -> AgentResult<Self> {
        if !result.valid_for(call) {
            return Err(AgentError::new(
                AgentErrorCode::ToolFailed,
                "The tool returned an invalid result.",
            ));
        }
        let observation = Self {
            tool: call.tool(),
            outcome: ToolOutcome::Success { result },
        };
        let bytes = serde_json::to_vec(&observation).map_err(|_| invalid_result())?;
        if bytes.len() > MAX_OBSERVATION_BYTES {
            return Err(invalid_result());
        }
        Ok(observation)
    }

    pub fn failure(call: &ValidatedCall, error: AgentError) -> AgentResult<Self> {
        let observation = Self {
            tool: call.tool(),
            outcome: ToolOutcome::Failure { error },
        };
        let bytes = serde_json::to_vec(&observation).map_err(|_| invalid_result())?;
        if bytes.len() > MAX_OBSERVATION_BYTES {
            return Err(invalid_result());
        }
        Ok(observation)
    }

    pub const fn tool(&self) -> ToolId {
        self.tool
    }

    /// Recheck the trusted adapter's result against this exact request.
    pub fn matches_call(&self, call: &ValidatedCall) -> bool {
        self.tool == call.tool()
            && match &self.outcome {
                ToolOutcome::Success { result } => result.valid_for(call),
                ToolOutcome::Failure { .. } => true,
            }
    }

    pub fn labels(&self) -> crate::classification::Labels {
        use crate::classification::{AGENT_CONTEXT, Labels, PDF_EXCERPT, TEXT_EXCERPT};
        let mut labels = Labels::from_source(AGENT_CONTEXT);
        match &self.outcome {
            ToolOutcome::Success {
                result: ToolOutput::PdfRead { .. },
            } => labels.insert(PDF_EXCERPT),
            ToolOutcome::Success {
                result: ToolOutput::FileRead { .. },
            } => labels.insert(TEXT_EXCERPT),
            _ => {}
        }
        labels
    }

    pub fn outcome(&self) -> &ToolOutcome {
        &self.outcome
    }
}

fn invalid_result() -> AgentError {
    AgentError::new(
        AgentErrorCode::ToolFailed,
        "The tool observation exceeded its contract.",
    )
}
