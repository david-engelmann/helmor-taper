//! Assertion record + the `result.json` shape `Tape::finish` writes.
//!
//! Kept structurally identical to the TypeScript implementation's
//! result.json so downstream tooling (the per-tape README templates,
//! the helmor-taper review viewer) can read either implementation's
//! output transparently during the migration.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ContinuousBeat;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Assertion {
    pub name: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Top-level `result.json` shape. `extras` is whatever the scenario
/// hands to `Tape::finish` — typically scenario-specific captures (the
/// container hostname, the laptop hostname, the count of DB rows, etc.)
/// that downstream assertions in the per-tape README cross-reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResultSummary {
    pub scenario: String,
    pub started_at: String,
    pub passed: bool,
    pub assertions: Vec<Assertion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub beats: Vec<ContinuousBeat>,
    /// Scenario-specific extras flattened at the top level — mirrors
    /// the TS port's `{...assertion-shape, ...extras}` spread.
    #[serde(flatten)]
    pub extras: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assertion_skips_empty_detail_field() {
        let a = Assertion {
            name: "ok".into(),
            ok: true,
            detail: String::new(),
        };
        let wire = serde_json::to_value(&a).unwrap();
        assert!(
            wire.get("detail").is_none(),
            "empty detail must not appear in result.json: {wire}"
        );
    }

    #[test]
    fn assertion_includes_detail_when_set() {
        let a = Assertion {
            name: "missing".into(),
            ok: false,
            detail: "selector did not appear in 15s".into(),
        };
        let wire = serde_json::to_value(&a).unwrap();
        assert_eq!(wire["detail"], "selector did not appear in 15s");
    }

    #[test]
    fn result_summary_round_trips_with_flattened_extras() {
        let summary = ResultSummary {
            scenario: "isolation-proof".into(),
            started_at: "2026-06-07T12:34:56.789Z".into(),
            passed: true,
            assertions: vec![Assertion {
                name: "hostname_arrived".into(),
                ok: true,
                detail: String::new(),
            }],
            beats: vec![ContinuousBeat {
                t: 1.2,
                caption: "ssh check".into(),
            }],
            extras: json!({"containerHostname": "081e3cab7eb5"}),
        };
        let wire = serde_json::to_string(&summary).unwrap();
        let back: ResultSummary = serde_json::from_str(&wire).unwrap();
        assert_eq!(summary, back);
        // Extras flattened — the container hostname appears at the top
        // level, NOT nested under "extras".
        let parsed: Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed["containerHostname"], "081e3cab7eb5");
        assert!(parsed.get("extras").is_none());
    }

    #[test]
    fn result_summary_omits_empty_beats() {
        let summary = ResultSummary {
            scenario: "scene-mode".into(),
            started_at: "2026-06-07T12:34:56.789Z".into(),
            passed: true,
            assertions: vec![],
            beats: vec![],
            extras: json!({}),
        };
        let wire = serde_json::to_value(&summary).unwrap();
        assert!(
            wire.get("beats").is_none(),
            "empty beats array must not serialise: {wire}"
        );
    }
}
