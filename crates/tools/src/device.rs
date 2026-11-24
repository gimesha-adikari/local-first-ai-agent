//! Linux hardware snapshot from fixed local sources. No subprocesses.

use std::{fs::File, io::{self, Read}, mem::MaybeUninit};

use agent_core::agent::{AgentError, AgentErrorCode, AgentResult, DeviceInfo};

use crate::ToolContext;

const CPUINFO_LIMIT: usize = 128 * 1024;
const MEMINFO_LIMIT: usize = 16 * 1024;

pub(crate) fn collect(context: &ToolContext<'_>) -> AgentResult<DeviceInfo> {
    context.check()?;
    let cpuinfo = read_prefix("/proc/cpuinfo", CPUINFO_LIMIT, context)?;
    let cpu = cpuinfo.as_deref().and_then(cpu_model).unwrap_or_else(|| "Unknown CPU".into());
    let logical_cpus = std::thread::available_parallelism()
        .ok()
        .and_then(|count| u32::try_from(count.get()).ok())
        .ok_or_else(unavailable)?;
    let meminfo = read_prefix("/proc/meminfo", MEMINFO_LIMIT, context)?.ok_or_else(unavailable)?;
    let (memory_total_bytes, memory_available_bytes) = memory(&meminfo).ok_or_else(unavailable)?;
    context.check()?;
    let storage = root_storage();
    context.check()?;
    Ok(DeviceInfo {
        os: "Linux".into(),
        architecture: std::env::consts::ARCH.into(),
        cpu,
        logical_cpus,
        memory_total_bytes,
        memory_available_bytes,
        storage_total_bytes: storage.map(|(total, _)| total),
        storage_available_bytes: storage.map(|(_, available)| available),
        // No reliable portable name source is assumed. GPU reporting is optional.
        gpu: None,
    })
}

/// Read a bounded prefix of fixed procfs entries. Never use model paths.
fn read_prefix(path: &'static str, limit: usize, context: &ToolContext<'_>) -> AgentResult<Option<String>> {
    context.check()?;
    let Ok(mut file) = File::open(path) else { return Ok(None) };
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];
    while output.len() < limit {
        context.check()?;
        let size = buffer.len().min(limit - output.len());
        match file.read(&mut buffer[..size]) {
            Ok(0) => break,
            Ok(n) => output.extend_from_slice(&buffer[..n]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Ok(None),
        }
    }
    context.check()?;
    // Ignore a potentially partial final line at the byte limit.
    if output.len() == limit {
        output.truncate(output.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1));
    }
    Ok(String::from_utf8(output).ok())
}

fn cpu_model(input: &str) -> Option<String> {
    // Only descriptive CPU fields; never return serials, machine IDs or flags.
    for key in ["model name", "Processor", "Hardware", "cpu model"] {
        for line in input.lines() {
            let Some((name, value)) = line.split_once(':') else { continue };
            let value = value.trim();
            if name.trim() == key
                && !value.is_empty()
                && value.len() <= 256
                && !value.chars().any(char::is_control)
            {
                return Some(value.to_owned());
            }
        }
    }
    None
}

fn memory(input: &str) -> Option<(u64, Option<u64>)> {
    fn kib(input: &str, wanted: &str) -> Option<u64> {
        let (_, value) = input.lines().filter_map(|line| line.split_once(':'))
            .find(|(key, _)| *key == wanted)?;
        let mut fields = value.split_whitespace();
        let size = fields.next()?.parse::<u64>().ok()?;
        if fields.next()? != "kB" || fields.next().is_some() {
            return None;
        }
        size.checked_mul(1024)
    }
    let total = kib(input, "MemTotal").filter(|total| *total > 0)?;
    let available = kib(input, "MemAvailable").filter(|available| *available <= total);
    Some((total, available))
}

fn root_storage() -> Option<(u64, u64)> {
    let mut output = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: c"/" is a valid NUL-terminated path. statvfs writes to an
    // aligned, properly sized buffer; read it only after success.
    let result = unsafe { libc::statvfs(c"/".as_ptr(), output.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    // SAFETY: statvfs reported success and initialized the structure.
    let stat = unsafe { output.assume_init() };
    let unit = u128::from(if stat.f_frsize == 0 { stat.f_bsize } else { stat.f_frsize });
    if unit == 0 {
        return None;
    }
    let total = u64::try_from(u128::from(stat.f_blocks).checked_mul(unit)?).ok()?;
    let available = u64::try_from(u128::from(stat.f_bavail).checked_mul(unit)?).ok()?;
    (available <= total).then_some((total, available))
}

fn unavailable() -> AgentError {
    AgentError::new(AgentErrorCode::ToolFailed, "Required device information is unavailable.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_parser_only_selects_descriptive_fields() {
        assert_eq!(cpu_model("Serial : secret\nmodel name : Example CPU\n"), Some("Example CPU".into()));
        assert_eq!(cpu_model("Serial : secret\nflags : example\n"), None);
    }

    #[test]
    fn memory_parser_validates_units_and_bounds() {
        assert_eq!(memory("MemTotal: 1024 kB\nMemAvailable: 512 kB\n"), Some((1_048_576, Some(524_288))));
        assert_eq!(memory("MemTotal: 1024 kB\nMemAvailable: 2048 kB\n"), Some((1_048_576, None)));
        assert_eq!(memory("MemTotal: 1024 kB\n"), Some((1_048_576, None)));
        assert_eq!(memory("MemTotal: 0 kB\n"), None);
        assert_eq!(memory("MemTotal: 18446744073709551615 kB\n"), None);
        assert_eq!(memory("MemTotal: 1024 MB\n"), None);
    }
}
