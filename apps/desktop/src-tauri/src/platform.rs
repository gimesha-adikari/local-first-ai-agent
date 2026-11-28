//! Tauri and Linux services for the platform-independent engine.

use engine::{BackgroundTask, BlockingTask, Platform};

use tauri::{AppHandle, Emitter};

pub struct TauriPlatform(pub AppHandle);

impl Platform for TauriPlatform {
    fn spawn_blocking(&self, task: BlockingTask) {
        // A panicking task drops its reply, which the engine reports.
        drop(tauri::async_runtime::spawn_blocking(task));
    }

    fn spawn(&self, task: BackgroundTask) {
        drop(tauri::async_runtime::spawn(task));
    }

    fn model_files(
        &self,
        path: &std::path::Path,
    ) -> std::io::Result<Box<dyn engine::downloads::ModelFiles>> {
        crate::model_files::PrivateModelFiles::open(path)
    }

    fn model_library_changed(&self) {
        let _ = self.0.emit_to("main", "model-library-changed", ());
    }

    fn available_memory(&self) -> Option<u64> {
        available_memory()
    }
}

/// MemAvailable from Linux procfs. It includes reclaimable cache, such as
/// mapped model pages.
pub fn available_memory() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = info
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))?;
    let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
    kib.checked_mul(1024)
}
