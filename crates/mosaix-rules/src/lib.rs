//! Window rule matching, precedence resolution, and evaluation explanations.

mod action;
mod builtin;
mod config;
mod evaluator;
mod matcher;
mod rule;
mod trace;

pub use action::{ManageAction, RuleActions};
pub use builtin::builtin_rules;
pub use config::{ActionConfig, MatcherConfig, RuleConfig, RuleConfigError};
pub use evaluator::RuleEvaluator;
pub use matcher::{FieldResult, WindowMatcher};
pub use rule::Rule;
pub use trace::{EvaluationResult, RuleTraceEntry};
