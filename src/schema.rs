//! Best-effort JSON-schema validation against the bundled LDtk schema.
//!
//! The official schema (`JSON_SCHEMA.json`) is intentionally loose, so violations are
//! reported as non-fatal warnings; the structural checks in `validate_project` remain the
//! authoritative gate.

use std::sync::OnceLock;

use jsonschema::Validator;
use serde_json::Value;

const SCHEMA_SRC: &str = include_str!("../JSON_SCHEMA.json");

fn validator() -> &'static Result<Validator, String> {
    static V: OnceLock<Result<Validator, String>> = OnceLock::new();
    V.get_or_init(|| {
        let schema: Value = serde_json::from_str(SCHEMA_SRC).map_err(|e| e.to_string())?;
        jsonschema::validator_for(&schema).map_err(|e| e.to_string())
    })
}

/// Validate an instance, returning a list of human-readable violation strings,
/// or `Err` if the schema itself could not be compiled.
pub fn validate(instance: &Value) -> Result<Vec<String>, String> {
    match validator() {
        Ok(v) => Ok(v
            .iter_errors(instance)
            .map(|e| format!("{} (at {})", e, e.instance_path()))
            .collect()),
        Err(e) => Err(e.clone()),
    }
}

/// The schema version string, for display.
pub fn schema_version() -> String {
    serde_json::from_str::<Value>(SCHEMA_SRC)
        .ok()
        .and_then(|v| v.get("version").and_then(|s| s.as_str()).map(String::from))
        .unwrap_or_else(|| "?".into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn schema_compiles() {
        // If the bundled schema fails to compile, every validate call would error.
        assert!(
            validator().is_ok(),
            "bundled schema failed to compile: {:?}",
            validator()
        );
    }

    #[test]
    fn schema_version_is_non_placeholder() {
        let v = schema_version();
        assert_ne!(v, "?", "expected a real version string from the bundled schema");
    }

    #[test]
    fn validate_returns_warnings_list() {
        // The schema is loose, so we only assert the call succeeds and returns a Vec.
        let warns = validate(&json!({})).expect("validation should run");
        let _ = warns.len();
    }
}
