//! Evaluation trace types.

use crate::action::RuleActions;
use crate::matcher::FieldResult;

/// One rule's evaluation against one window.
#[derive(Debug, Clone)]
pub struct RuleTraceEntry {
    pub rule_id: String,
    pub priority: i32,
    pub enabled: bool,
    pub matched: bool,
    /// Per-field results showing which conditions matched/failed.
    pub field_results: Vec<FieldResult>,
}

/// Complete evaluation result for one window.
#[derive(Debug, Clone)]
pub struct EvaluationResult {
    /// The resolved action (from the winning rule, or default Tile).
    pub actions: RuleActions,
    /// Which rule produced the action, if any.
    pub matching_rule_id: Option<String>,
    /// Every rule evaluated, in evaluation order, with full field-level detail.
    pub trace: Vec<RuleTraceEntry>,
}
