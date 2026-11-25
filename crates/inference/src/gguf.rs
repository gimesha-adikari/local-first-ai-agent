use std::{fs::File, io::Read, path::Path};

use agent_core::inference::{ErrorCode, InferenceError, Result};

#[cfg(feature = "llama")]
pub fn trained_context(path: &Path) -> Result<(u32, u64)> {
    use llama_cpp_2::gguf::GgufContext;

    // Type tags defined by the GGUF format. Native getters require an exact
    // type match and can abort if called with a different type.
    const UINT32: u32 = 4;
    const STRING: u32 = 8;
    let bytes = validate(path)?;
    let context = (|| {
        let metadata = GgufContext::from_file(path)?;
        let architecture = metadata.find_key("general.architecture");
        if architecture < 0 || metadata.kv_type(architecture) != STRING {
            return None;
        }
        let architecture = metadata.val_str(architecture)?;
        let length = metadata.find_key(&format!("{architecture}.context_length"));
        if length < 0 || metadata.kv_type(length) != UINT32 {
            return None;
        }
        let length = metadata.val_u32(length);
        (length >= 128).then_some(length)
    })()
    .ok_or_else(|| {
        InferenceError::new(
            ErrorCode::InvalidModel,
            "Could not read a supported context length from this GGUF model.",
        )
    })?;
    Ok((context, bytes))
}

pub(crate) fn validate(path: &Path) -> Result<u64> {
    let invalid_path = || {
        InferenceError::new(
            ErrorCode::InvalidPath,
            "Choose a readable, regular .gguf file.",
        )
    };
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
    {
        return Err(invalid_path());
    }
    let metadata = path.metadata().map_err(|_| invalid_path())?;
    if !metadata.is_file() {
        return Err(invalid_path());
    }
    if let Some((part, total)) = path
        .file_stem()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit_once("-of-"))
        && let Some((_, part)) = part.rsplit_once('-')
        && [part, total]
            .iter()
            .all(|part| part.len() == 5 && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(InferenceError::new(
            ErrorCode::InvalidModel,
            "Choose a single-file GGUF model. Split models are unsupported.",
        ));
    }
    let mut header = [0; 24];
    let mut file = File::open(path).map_err(|_| invalid_path())?;
    if file.read_exact(&mut header).is_err()
        || &header[..4] != b"GGUF"
        || !matches!(&header[4..8], [2 | 3, 0, 0, 0])
    {
        return Err(InferenceError::new(
            ErrorCode::InvalidModel,
            "The selected file has an invalid or unsupported GGUF header.",
        ));
    }
    Ok(metadata.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_paths_and_files() {
        let dir =
            std::env::temp_dir().join(format!("agent-gguf-validation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("missing.gguf");
        assert_eq!(validate(&path).unwrap_err().code, ErrorCode::InvalidPath);
        assert_eq!(validate(&dir).unwrap_err().code, ErrorCode::InvalidPath);
        std::fs::write(&path, b"GGUF").unwrap();
        assert_eq!(validate(&path).unwrap_err().code, ErrorCode::InvalidModel);
        std::fs::write(&path, [0; 24]).unwrap();
        assert_eq!(validate(&path).unwrap_err().code, ErrorCode::InvalidModel);
        let split = dir.join("model-00001-of-00002.gguf");
        std::fs::write(&split, [0; 24]).unwrap();
        assert!(validate(&split).unwrap_err().message.contains("Split"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
