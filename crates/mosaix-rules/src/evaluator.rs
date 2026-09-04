//! Rule evaluator.

use crate::action::RuleActions;
use crate::rule::Rule;
use crate::trace::{EvaluationResult, RuleTraceEntry};
use mosaix_domain::Window;

/// Evaluates rules against windows to determine management actions.
pub struct RuleEvaluator {
    rules: Vec<Rule>,
}

impl RuleEvaluator {
    /// Creates a new rule evaluator with the given rules.
    pub fn new(mut rules: Vec<Rule>) -> Self {
        rules.sort_by_key(|rule| std::cmp::Reverse(rule.priority));
        Self { rules }
    }

    /// Evaluates the rules against a window to find the highest-priority match.
    pub fn evaluate(&self, window: &Window) -> EvaluationResult {
        let mut trace = Vec::new();
        let mut matched_rule = None;
        let mut resolved_actions = RuleActions::default();

        for rule in &self.rules {
            let field_results = rule.matcher.matches_detailed(window);
            let matched = rule.enabled && field_results.iter().all(|f| f.matched);

            trace.push(RuleTraceEntry {
                rule_id: rule.id.clone(),
                priority: rule.priority,
                enabled: rule.enabled,
                matched,
                field_results,
            });

            if matched && matched_rule.is_none() {
                matched_rule = Some(rule.id.clone());
                resolved_actions = rule.actions.clone();
            }
        }

        EvaluationResult {
            actions: resolved_actions,
            matching_rule_id: matched_rule,
            trace,
        }
    }

    /// Adds new rules and re-sorts the list.
    pub fn add_rules(&mut self, mut new_rules: Vec<Rule>) {
        self.rules.append(&mut new_rules);
        self.rules
            .sort_by_key(|rule| std::cmp::Reverse(rule.priority));
    }

    /// Returns a reference to the loaded rules.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::ManageAction;
    use crate::matcher::WindowMatcher;
    use mosaix_domain::{
        ApplicationId, DisplayId, Rect, WindowCapabilities, WindowId, WindowLifecycle, WindowRole,
    };
    use std::path::PathBuf;

    fn create_window() -> Window {
        Window {
            id: WindowId(1),
            process_id: 1234,
            application_id: ApplicationId("test".to_string()),
            executable_path: Some(PathBuf::from("/test")),
            title: "test".to_string(),
            native_class: Some("test".to_string()),
            role: WindowRole::Normal,
            bounds: Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
            display_id: DisplayId(1),
            capabilities: WindowCapabilities {
                can_move: true,
                can_resize: true,
                can_minimize: true,
                can_maximize: true,
            },
            elevated: false,
            lifecycle: WindowLifecycle::Active,
            minimum_size: None,
        }
    }

    fn create_rule(
        id: &str,
        priority: i32,
        enabled: bool,
        match_app_id: Option<&str>,
        action: ManageAction,
    ) -> Rule {
        Rule {
            id: id.to_string(),
            priority,
            enabled,
            matcher: WindowMatcher {
                application_id: match_app_id.map(|s| s.to_string()),
                application_regex: None,
                title_regex: None,
                native_class: None,
                class_regex: None,
                exe_path_regex: None,
                exe_path: None,
                role: None,
            },
            actions: RuleActions {
                manage: action,
                workspace: None,
            },
        }
    }

    #[test]
    fn test_no_rules() {
        let evaluator = RuleEvaluator::new(vec![]);
        let result = evaluator.evaluate(&create_window());
        assert_eq!(result.actions.manage, ManageAction::Tile);
        assert_eq!(result.matching_rule_id, None);
        assert!(result.trace.is_empty());
    }

    #[test]
    fn test_single_matching_rule() {
        let rule = create_rule("rule1", 0, true, Some("test"), ManageAction::Float);
        let evaluator = RuleEvaluator::new(vec![rule]);
        let result = evaluator.evaluate(&create_window());
        assert_eq!(result.actions.manage, ManageAction::Float);
        assert_eq!(result.matching_rule_id, Some("rule1".to_string()));
        assert_eq!(result.trace.len(), 1);
        assert!(result.trace[0].matched);
    }

    #[test]
    fn test_single_non_matching_rule() {
        let rule = create_rule("rule1", 0, true, Some("other"), ManageAction::Float);
        let evaluator = RuleEvaluator::new(vec![rule]);
        let result = evaluator.evaluate(&create_window());
        assert_eq!(result.actions.manage, ManageAction::Tile);
        assert_eq!(result.matching_rule_id, None);
        assert_eq!(result.trace.len(), 1);
        assert!(!result.trace[0].matched);
    }

    #[test]
    fn test_multiple_rules_highest_priority_wins() {
        let rule1 = create_rule("rule1", 10, true, Some("test"), ManageAction::Float);
        let rule2 = create_rule("rule2", 20, true, Some("test"), ManageAction::Exclude);
        let evaluator = RuleEvaluator::new(vec![rule1, rule2]);
        let result = evaluator.evaluate(&create_window());
        assert_eq!(result.actions.manage, ManageAction::Exclude);
        assert_eq!(result.matching_rule_id, Some("rule2".to_string()));
    }

    #[test]
    fn test_disabled_rule_is_skipped() {
        let rule1 = create_rule("rule1", 20, false, Some("test"), ManageAction::Exclude);
        let rule2 = create_rule("rule2", 10, true, Some("test"), ManageAction::Float);
        let evaluator = RuleEvaluator::new(vec![rule1, rule2]);
        let result = evaluator.evaluate(&create_window());
        assert_eq!(result.actions.manage, ManageAction::Float);
        assert_eq!(result.matching_rule_id, Some("rule2".to_string()));
        assert_eq!(result.trace.len(), 2);
        assert!(!result.trace[0].matched);
        assert!(result.trace[1].matched);
    }
}
