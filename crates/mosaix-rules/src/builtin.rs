//! Built-in rules.

use crate::action::{ManageAction, RuleActions};
use crate::matcher::WindowMatcher;
use crate::rule::Rule;
use mosaix_domain::WindowRole;
use regex::Regex;

/// Returns the sensible default built-in rules for Mosaix.
pub fn builtin_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "builtin-dialogs".to_string(),
            priority: -100,
            enabled: true,
            matcher: WindowMatcher {
                application_id: None,
                application_regex: None,
                title_regex: None,
                native_class: None,
                class_regex: None,
                exe_path_regex: None,
                exe_path: None,
                role: Some(WindowRole::Dialog),
            },
            actions: RuleActions {
                manage: ManageAction::Float,
            },
        },
        Rule {
            id: "builtin-tool-windows".to_string(),
            priority: -100,
            enabled: true,
            matcher: WindowMatcher {
                application_id: None,
                application_regex: None,
                title_regex: None,
                native_class: None,
                class_regex: None,
                exe_path_regex: None,
                exe_path: None,
                role: Some(WindowRole::ToolWindow),
            },
            actions: RuleActions {
                manage: ManageAction::Float,
            },
        },
        Rule {
            id: "builtin-popups".to_string(),
            priority: -100,
            enabled: true,
            matcher: WindowMatcher {
                application_id: None,
                application_regex: None,
                title_regex: None,
                native_class: None,
                class_regex: None,
                exe_path_regex: None,
                exe_path: None,
                role: Some(WindowRole::Popup),
            },
            actions: RuleActions {
                manage: ManageAction::Exclude,
            },
        },
        Rule {
            id: "builtin-splash".to_string(),
            priority: -100,
            enabled: true,
            matcher: WindowMatcher {
                application_id: None,
                application_regex: None,
                title_regex: None,
                native_class: None,
                class_regex: None,
                exe_path_regex: None,
                exe_path: None,
                role: Some(WindowRole::Splash),
            },
            actions: RuleActions {
                manage: ManageAction::Exclude,
            },
        },
        Rule {
            id: "builtin-pip-chromium".to_string(),
            priority: -100,
            enabled: true,
            matcher: WindowMatcher {
                application_id: None,
                application_regex: None,
                title_regex: Some(
                    Regex::new("(?i)picture.in.picture").expect("builtin regex compile"),
                ),
                native_class: None,
                class_regex: None,
                exe_path_regex: None,
                exe_path: None,
                role: None,
            },
            actions: RuleActions {
                manage: ManageAction::Float,
            },
        },
        Rule {
            id: "builtin-game-launchers".to_string(),
            priority: -100,
            enabled: true,
            matcher: WindowMatcher {
                application_id: None,
                application_regex: Some(
                    Regex::new(r"(?i)(steam|epicgames|origin|battle\.net|gog)")
                        .expect("builtin regex compile"),
                ),
                title_regex: None,
                native_class: None,
                class_regex: None,
                exe_path_regex: None,
                exe_path: None,
                role: None,
            },
            actions: RuleActions {
                manage: ManageAction::Exclude,
            },
        },
        Rule {
            id: "builtin-screen-share".to_string(),
            priority: -100,
            enabled: true,
            matcher: WindowMatcher {
                application_id: None,
                application_regex: None,
                title_regex: Some(
                    Regex::new(r"(?i)(screen.shar|is sharing|sharing your screen)")
                        .expect("builtin regex compile"),
                ),
                native_class: None,
                class_regex: None,
                exe_path_regex: None,
                exe_path: None,
                role: None,
            },
            actions: RuleActions {
                manage: ManageAction::Exclude,
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaix_domain::{
        ApplicationId, DisplayId, Rect, Window, WindowCapabilities, WindowId, WindowLifecycle,
    };
    use std::path::PathBuf;

    fn create_window(role: WindowRole) -> Window {
        Window {
            id: WindowId(1),
            process_id: 1234,
            application_id: ApplicationId("test".to_string()),
            executable_path: Some(PathBuf::from("/test")),
            title: "test".to_string(),
            native_class: Some("test".to_string()),
            role,
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

    #[test]
    fn test_builtin_rules_count() {
        let rules = builtin_rules();
        assert_eq!(rules.len(), 7);
    }

    #[test]
    fn test_builtin_rules_unique_ids() {
        let rules = builtin_rules();
        let ids: std::collections::HashSet<_> = rules.iter().map(|r| r.id.clone()).collect();
        assert_eq!(ids.len(), 7);
    }

    #[test]
    fn test_builtin_dialog_rule() {
        let rules = builtin_rules();
        let dialog_rule = rules.iter().find(|r| r.id == "builtin-dialogs").unwrap();
        assert!(dialog_rule
            .matcher
            .matches(&create_window(WindowRole::Dialog)));
        assert!(!dialog_rule
            .matcher
            .matches(&create_window(WindowRole::Normal)));
        assert_eq!(dialog_rule.actions.manage, ManageAction::Float);
    }

    #[test]
    fn test_builtin_popup_rule() {
        let rules = builtin_rules();
        let popup_rule = rules.iter().find(|r| r.id == "builtin-popups").unwrap();
        assert!(popup_rule
            .matcher
            .matches(&create_window(WindowRole::Popup)));
        assert!(!popup_rule
            .matcher
            .matches(&create_window(WindowRole::Normal)));
        assert_eq!(popup_rule.actions.manage, ManageAction::Exclude);
    }
}
