//! Rule definition.

use crate::action::RuleActions;
use crate::matcher::WindowMatcher;

/// A single window management rule.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Unique identifier for this rule.
    pub id: String,
    /// Higher priority wins. User rules default to 0, built-ins to -100.
    pub priority: i32,
    /// Whether this rule is active.
    pub enabled: bool,
    /// Conditions the window must satisfy.
    pub matcher: WindowMatcher,
    /// Actions to apply when all conditions match.
    pub actions: RuleActions,
}
