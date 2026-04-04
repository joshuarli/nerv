pub mod codemap;
pub mod diff;
pub mod edit;
pub mod epsh;
pub mod file_mutation_queue;
pub mod find;
pub mod grep;
pub mod ls;
pub mod memory;
pub mod output_filter;
pub mod read;
pub mod symbols;
pub mod truncate;
pub mod write;

pub use codemap::CodemapTool;
pub use edit::EditTool;
pub use epsh::EpshTool;
pub use file_mutation_queue::FileMutationQueue;
pub use find::FindTool;
pub use grep::GrepTool;
pub use ls::LsTool;
pub use memory::MemoryTool;
pub use read::ReadTool;
pub use symbols::SymbolsTool;
pub use write::WriteTool;

/// Reject any JSON object key not in `allowed`. Returns a sorted, deterministic
/// error listing the unknown keys — makes mistyped argument names actionable.
pub fn validate_known_keys(input: &serde_json::Value, allowed: &[&str]) -> Result<(), crate::errors::ToolError> {
    let Some(obj) = input.as_object() else {
        return Err(crate::errors::ToolError::InvalidArguments {
            message: "arguments must be an object".into(),
        });
    };
    let mut unknown: Vec<&str> =
        obj.keys().map(|k| k.as_str()).filter(|k| !allowed.contains(k)).collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    Err(crate::errors::ToolError::InvalidArguments {
        message: format!(
            "unknown argument(s): {} (allowed: {})",
            unknown.join(", "),
            allowed.join(", "),
        ),
    })
}
