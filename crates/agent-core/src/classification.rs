//! Classification metadata. Adapters and IPC supply content; trusted Rust
//! ingress and storage code attach labels according to the source.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::capability::valid_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    Public,
    Content,
    Personal,
    OtherPeople,
    RawSensor,
    Secret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataClass {
    pub id: &'static str,
    pub sensitivity: Sensitivity,
}

pub const USER_INPUT: DataClass = personal("core.user-input/1");
pub const CLIPBOARD_TEXT: DataClass = personal("core.clipboard-text/1");
/// A user-selected crop of one captured screen frame. Full frames never
/// leave the desktop host and have no class of their own.
pub const SCREEN_IMAGE: DataClass = DataClass {
    id: "core.screen-image/1",
    sensitivity: Sensitivity::RawSensor,
};
/// Text recognized from a screen crop. It also carries the crop's labels:
/// recognition does not declassify.
pub const SCREEN_TEXT: DataClass = personal("core.screen-text/1");
pub const HISTORY: DataClass = personal("core.history/1");
pub const MEMORY: DataClass = personal("core.memory/1");
pub const LIBRARY: DataClass = personal("core.library/1");
pub const PDF_EXCERPT: DataClass = personal("core.pdf-excerpt/1");
pub const TEXT_EXCERPT: DataClass = personal("core.text-excerpt/1");
pub const AGENT_CONTEXT: DataClass = personal("core.agent-context/1");
pub const EMBEDDING: DataClass = personal("core.embedding/1");
pub const QUERY: DataClass = personal("core.query/1");
pub const MODEL_OUTPUT: DataClass = personal("core.model-output/1");
pub const SYSTEM_TEXT: DataClass = DataClass {
    id: "core.system-text/1",
    sensitivity: Sensitivity::Content,
};
/// Requests that carry no personal content, such as model discovery and
/// connection tests.
pub const PROVIDER_CONTROL: DataClass = DataClass {
    id: "core.provider-control/1",
    sensitivity: Sensitivity::Public,
};
pub const MODEL_ARTIFACT: DataClass = DataClass {
    id: "core.model-artifact/1",
    sensitivity: Sensitivity::Public,
};
/// Untrusted public web material: search results, public page URLs found
/// through search, and text extracted from public webpages. It is public in
/// origin but never trusted as instructions, and it does not declassify any
/// personal input it is combined with.
pub const PUBLIC_WEB: DataClass = DataClass {
    id: "core.public-web/1",
    sensitivity: Sensitivity::Public,
};
/// The source class of a file the user attached to a Chat message. Attached
/// files are untrusted data from the user's device (Stage 04-D).
pub const ATTACHMENT: DataClass = personal("core.attachment/1");
/// Text extracted from an attached document, or recognized from an attached
/// image. It also carries the attachment's other labels.
pub const ATTACHMENT_TEXT: DataClass = personal("core.attachment-text/1");
/// A validated, metadata-stripped image representation of an attachment.
/// It is user-chosen file content, not a sensor capture.
pub const ATTACHMENT_IMAGE: DataClass = personal("core.attachment-image/1");
/// A camera snapshot the user explicitly captured and confirmed (Stage
/// 04-E). Like a screen crop it is a raw sensor capture: it may show
/// people, places and documents, and it never declassifies.
pub const CAMERA_IMAGE: DataClass = DataClass {
    id: "core.camera-image/1",
    sensitivity: Sensitivity::RawSensor,
};
/// Text recognized from a camera snapshot. It also carries the snapshot's
/// labels: recognition does not declassify.
pub const CAMERA_TEXT: DataClass = personal("core.camera-text/1");
pub const SECRET: DataClass = DataClass {
    id: "core.secret/1",
    sensitivity: Sensitivity::Secret,
};

const fn personal(id: &'static str) -> DataClass {
    DataClass {
        id,
        sensitivity: Sensitivity::Personal,
    }
}

#[derive(Debug, Clone)]
pub struct DataClasses(BTreeMap<&'static str, Sensitivity>);

impl Default for DataClasses {
    fn default() -> Self {
        Self::new([
            USER_INPUT,
            CLIPBOARD_TEXT,
            SCREEN_IMAGE,
            SCREEN_TEXT,
            HISTORY,
            MEMORY,
            LIBRARY,
            PDF_EXCERPT,
            TEXT_EXCERPT,
            AGENT_CONTEXT,
            EMBEDDING,
            QUERY,
            MODEL_OUTPUT,
            SYSTEM_TEXT,
            PROVIDER_CONTROL,
            MODEL_ARTIFACT,
            PUBLIC_WEB,
            ATTACHMENT,
            ATTACHMENT_TEXT,
            ATTACHMENT_IMAGE,
            CAMERA_IMAGE,
            CAMERA_TEXT,
            SECRET,
        ])
    }
}

impl DataClasses {
    pub fn new(classes: impl IntoIterator<Item = DataClass>) -> Self {
        let mut known = BTreeMap::new();
        let mut conflicts = BTreeSet::new();
        for class in classes.into_iter().filter(|class| valid_id(class.id)) {
            if known
                .insert(class.id, class.sensitivity)
                .is_some_and(|family| family != class.sensitivity)
            {
                conflicts.insert(class.id);
            }
        }
        // Conflicting registrations have no authoritative classification.
        known.retain(|id, _| !conflicts.contains(id));
        Self(known)
    }

    pub fn sensitivity(&self, id: &str) -> Option<Sensitivity> {
        self.0.get(id).copied()
    }
}

/// A deterministic union of origin classes. Each class retains its own
/// sensitivity family. Even a lower-sensitivity derived class cannot remove
/// an input's higher-sensitivity class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Labels(BTreeSet<String>);

impl Labels {
    pub fn from_source(class: DataClass) -> Self {
        Self(BTreeSet::from([class.id.to_owned()]))
    }

    pub fn insert(&mut self, class: DataClass) {
        self.0.insert(class.id.to_owned());
    }

    pub fn inherit(&mut self, input: &Self) {
        self.0.extend(input.0.iter().cloned());
    }

    pub fn derived(inputs: impl IntoIterator<Item = Self>, class: DataClass) -> Self {
        let mut labels = Self::from_source(class);
        for input in inputs {
            labels.inherit(&input);
        }
        labels
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    pub fn contains(&self, class: DataClass) -> bool {
        self.0.contains(class.id)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// No deserialization constructor. Only trusted ingress or a transformation
/// in Rust can pair data with its provenance.
#[derive(Debug)]
pub struct Classified<T> {
    value: T,
    labels: Labels,
}

impl<T> Classified<T> {
    pub fn from_source(value: T, class: DataClass) -> Self {
        Self {
            value,
            labels: Labels::from_source(class),
        }
    }

    pub fn derived<'a>(
        value: T,
        inputs: impl IntoIterator<Item = &'a Labels>,
        class: DataClass,
    ) -> Self {
        Self {
            value,
            labels: Labels::derived(inputs.into_iter().cloned(), class),
        }
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn labels(&self) -> &Labels {
        &self.labels
    }

    pub fn into_parts(self) -> (T, Labels) {
        (self.value, self.labels)
    }

    pub fn map<U>(self, class: DataClass, transform: impl FnOnce(T) -> U) -> Classified<U> {
        Classified {
            value: transform(self.value),
            labels: Labels::derived([self.labels], class),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translation_summary_and_public_derivative_preserve_all_input_families() {
        let translated = DataClass {
            id: "test.translation/1",
            sensitivity: Sensitivity::Content,
        };
        let summary = DataClass {
            id: "test.summary/1",
            sensitivity: Sensitivity::Public,
        };
        let captured = DataClass {
            id: "test.capture/1",
            sensitivity: Sensitivity::RawSensor,
        };
        let source = Classified::from_source("synthetic", captured).map(translated, str::len);
        let result = Classified::derived(
            *source.value(),
            [source.labels(), &Labels::from_source(MEMORY)],
            summary,
        );
        for class in [captured, translated, MEMORY, summary] {
            assert!(result.labels().contains(class));
        }
        assert_eq!(
            DataClasses::new([captured, translated, MEMORY, summary]).sensitivity(captured.id),
            Some(Sensitivity::RawSensor)
        );
    }

    #[test]
    fn versioned_registry_allows_explicit_extensions_and_rejects_conflicts() {
        use crate::capability::{CapabilityId, CapabilityRegistry, GENERATE};
        let extension = CapabilityId::new("test.process/2");
        let registry =
            CapabilityRegistry::new([GENERATE, extension, CapabilityId::new("unversioned")]);
        assert_eq!(registry.resolve(extension.as_str()), Some(extension));
        assert_eq!(registry.resolve("test.process/1"), None);
        assert_eq!(registry.resolve("unversioned"), None);
        let conflicting = DataClass {
            sensitivity: Sensitivity::Public,
            ..MEMORY
        };
        assert_eq!(
            DataClasses::new([MEMORY, conflicting]).sensitivity(MEMORY.id),
            None
        );
    }
}
