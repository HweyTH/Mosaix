//! TOML-deserializable rule config.

use crate::action::{ManageAction, RuleActions};
use crate::matcher::WindowMatcher;
use crate::rule::Rule;
use mosaix_domain::WindowRole;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Error converting RuleConfig to Rule.
#[derive(Debug, thiserror::Error)]
pub enum RuleConfigError {
    #[error("Rule '{rule_id}' has an invalid regex in field '{field}': {source}")]
    InvalidRegex {
        rule_id: String,
        field: String,
        #[source]
        source: regex::Error,
    },
    #[error("Rule '{rule_id}' has an invalid role: {role}")]
    InvalidRole { rule_id: String, role: String },
}

/// A single rule as it appears in config.toml under `[[rules]]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleConfig {
    pub id: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(rename = "match")]
    pub matcher: MatcherConfig,
    #[serde(default)]
    pub actions: ActionConfig,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatcherConfig {
    pub application_id: Option<String>,
    pub application_regex: Option<String>,
    pub title_regex: Option<String>,
    pub native_class: Option<String>,
    pub class_regex: Option<String>,
    pub exe_path_regex: Option<String>,
    pub exe_path: Option<String>,
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionConfig {
    #[serde(default = "default_manage")]
    pub manage: ManageAction,
    /// The logical workspace a matching window joins. Names an existing
    /// workspace; an unknown name is a typed refusal at evaluation time,
    /// never an implicit creation (ADR 0028).
    #[serde(default)]
    pub workspace: Option<String>,
}

fn default_manage() -> ManageAction {
    ManageAction::Tile
}

impl Default for ActionConfig {
    fn default() -> Self {
        Self {
            manage: default_manage(),
            workspace: None,
        }
    }
}

impl TryFrom<RuleConfig> for Rule {
    type Error = RuleConfigError;

    fn try_from(config: RuleConfig) -> Result<Self, Self::Error> {
        let compile_regex = |field_name: &str,
                             pattern: &Option<String>|
         -> Result<Option<Regex>, RuleConfigError> {
            match pattern {
                Some(p) => Regex::new(p)
                    .map(Some)
                    .map_err(|e| RuleConfigError::InvalidRegex {
                        rule_id: config.id.clone(),
                        field: field_name.to_string(),
                        source: e,
                    }),
                None => Ok(None),
            }
        };

        let application_regex =
            compile_regex("application_regex", &config.matcher.application_regex)?;
        let title_regex = compile_regex("title_regex", &config.matcher.title_regex)?;
        let class_regex = compile_regex("class_regex", &config.matcher.class_regex)?;
        let exe_path_regex = compile_regex("exe_path_regex", &config.matcher.exe_path_regex)?;

        let role = match &config.matcher.role {
            Some(r) => {
                let parsed_role = match r.to_lowercase().as_str() {
                    "normal" => WindowRole::Normal,
                    "dialog" => WindowRole::Dialog,
                    "tool-window" => WindowRole::ToolWindow,
                    "popup" => WindowRole::Popup,
                    "splash" => WindowRole::Splash,
                    "unknown" => WindowRole::Unknown,
                    _ => {
                        return Err(RuleConfigError::InvalidRole {
                            rule_id: config.id.clone(),
                            role: r.clone(),
                        });
                    }
                };
                Some(parsed_role)
            }
            None => None,
        };

        Ok(Rule {
            id: config.id,
            priority: config.priority,
            enabled: config.enabled,
            matcher: WindowMatcher {
                application_id: config.matcher.application_id,
                application_regex,
                title_regex,
                native_class: config.matcher.native_class,
                class_regex,
                exe_path_regex,
                exe_path: config.matcher.exe_path,
                role,
            },
            actions: RuleActions {
                manage: config.actions.manage,
                workspace: config.actions.workspace,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_trip() {
        let toml_str = r#"
            id = "test-rule"
            priority = 10
            match.application_id = "com.test.app"
            actions.manage = "float"
        "#;
        let config: RuleConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.id, "test-rule");
        assert_eq!(config.priority, 10);
        assert!(config.enabled);
        assert_eq!(
            config.matcher.application_id.as_deref(),
            Some("com.test.app")
        );
        assert_eq!(config.actions.manage, ManageAction::Float);

        let rule: Rule = config.try_into().unwrap();
        assert_eq!(rule.id, "test-rule");
        assert_eq!(rule.priority, 10);
        assert!(rule.enabled);
        assert_eq!(rule.matcher.application_id.as_deref(), Some("com.test.app"));
        assert_eq!(rule.actions.manage, ManageAction::Float);
    }

    #[test]
    fn test_invalid_regex() {
        let toml_str = r#"
            id = "test-rule"
            match.title_regex = "(invalid"
        "#;
        let config: RuleConfig = toml::from_str(toml_str).unwrap();
        let result: Result<Rule, _> = config.try_into();
        assert!(
            matches!(result, Err(RuleConfigError::InvalidRegex { field, .. }) if field == "title_regex")
        );
    }

    #[test]
    fn test_invalid_role() {
        let toml_str = r#"
            id = "test-rule"
            match.role = "invalid-role"
        "#;
        let config: RuleConfig = toml::from_str(toml_str).unwrap();
        let result: Result<Rule, _> = config.try_into();
        assert!(
            matches!(result, Err(RuleConfigError::InvalidRole { role, .. }) if role == "invalid-role")
        );
    }

    #[test]
    fn test_minimal_config() {
        let toml_str = r#"
            id = "minimal"
            [match]
        "#;
        let config: RuleConfig = toml::from_str(toml_str).unwrap();
        let rule: Rule = config.try_into().unwrap();
        assert_eq!(rule.id, "minimal");
        assert_eq!(rule.priority, 0);
        assert!(rule.enabled);
        assert_eq!(rule.actions.manage, ManageAction::Tile);
    }
}
