//! Native tool adapters. Dispatch consumes an authorization from agent-core.

mod calculator;
#[cfg(target_os = "linux")]
mod device;
#[cfg(target_os = "linux")]
mod filesystem;

#[cfg(target_os = "linux")]
pub use filesystem::FilesystemScopes;

use std::time::{Duration, Instant};

use agent_core::agent::{
    AgentError, AgentErrorCode, AgentResult, AuthorizedCall, ToolArguments, ToolId,
    ToolObservation, ToolOutput, ToolRegistry,
};

/// The registry contains only capabilities this build can currently execute.
/// Future adapters will extend this allowlist, not expose all definitions.
pub fn supported_registry() -> AgentResult<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    registry.register(ToolId::Calculate)?;
    #[cfg(target_os = "linux")]
    {
        registry.register(ToolId::DeviceInfo)?;
        registry.register(ToolId::FileSearch)?;
        registry.register(ToolId::FileRead)?;
    }
    Ok(registry)
}

/// Session-owned native adapters and resource handles.
#[derive(Default)]
pub struct NativeTools {
    #[cfg(target_os = "linux")]
    filesystem: FilesystemScopes,
}

impl NativeTools {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(target_os = "linux")]
    pub fn filesystem(&self) -> &FilesystemScopes {
        &self.filesystem
    }

    /// Own the non-cloneable permit through dispatch. The host must persist the
    /// permission audit before calling this entry point and run it off the UI
    /// thread. This function performs no approval or resource selection.
    pub fn execute(
        &self,
        registry: &ToolRegistry,
        authorization: AuthorizedCall,
    ) -> AgentResult<ToolObservation> {
        let call = authorization.call();
        let definition = registry.get(call.tool()).ok_or_else(|| {
            AgentError::new(AgentErrorCode::UnknownTool, "The requested tool is not registered.")
        })?;
        authorization.check_dispatch()?;
        let context = ToolContext {
            authorization: &authorization,
            started: Instant::now(),
            timeout: Duration::from_millis(definition.timeout_ms),
        };

        let result = match (call.tool(), call.arguments()) {
            (ToolId::Calculate, ToolArguments::Calculate(args)) => {
                calculator::evaluate(&args.expression, || context.check())
                    .map(|value| ToolOutput::Calculate { value })
            }
            #[cfg(target_os = "linux")]
            (ToolId::DeviceInfo, ToolArguments::Empty {}) => {
                device::collect(&context).map(ToolOutput::DeviceInfo)
            }
            #[cfg(target_os = "linux")]
            (ToolId::FileSearch, ToolArguments::FileSearch(args)) => {
                self.filesystem.search(args, &context)
            }
            #[cfg(target_os = "linux")]
            (ToolId::FileRead, ToolArguments::FileRead(args)) => {
                self.filesystem.read(args, &context)
            }
            _ => Err(AgentError::new(
                AgentErrorCode::UnsupportedPlatform,
                "The requested tool has no native adapter in this build.",
            )),
        };

        // Never release a successful read after cancellation or deadline expiry.
        context.check()?;
        match result {
            Ok(output) => ToolObservation::success(call, output),
            Err(error) => ToolObservation::failure(call, error),
        }
    }

}

/// Compatibility entry point for stateless tools. Scoped operations require
/// the session-owned NativeTools instance and cannot use this empty store.
pub fn execute(
    registry: &ToolRegistry,
    authorization: AuthorizedCall,
) -> AgentResult<ToolObservation> {
    NativeTools::new().execute(registry, authorization)
}

struct ToolContext<'a> {
    authorization: &'a AuthorizedCall,
    started: Instant,
    timeout: Duration,
}

impl ToolContext<'_> {
    fn check(&self) -> AgentResult<()> {
        self.authorization.check_live()?;
        if self.started.elapsed() >= self.timeout {
            return Err(AgentError::new(AgentErrorCode::TimedOut, "The tool deadline expired."));
        }
        Ok(())
    }
}
