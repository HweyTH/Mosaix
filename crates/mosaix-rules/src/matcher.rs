//! Window matching logic.

use mosaix_domain::{Window, WindowRole};
use regex::Regex;

/// The result of matching a single field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldResult {
    /// The name of the field matched.
    pub field: String,
    /// The expected value or pattern.
    pub expected: String,
    /// The actual value from the window.
    pub actual: String,
    /// Whether the field matched.
    pub matched: bool,
}

/// A matcher that evaluates conditions against a window.
#[derive(Debug, Clone)]
pub struct WindowMatcher {
    /// Exact match against `Window::application_id.0`.
    pub application_id: Option<String>,
    /// Regex match against `Window::application_id.0`.
    pub application_regex: Option<Regex>,
    /// Regex match against `Window::title`.
    pub title_regex: Option<Regex>,
    /// Exact match against `Window::native_class`.
    pub native_class: Option<String>,
    /// Regex match against `Window::native_class`.
    pub class_regex: Option<Regex>,
    /// Regex match against `Window::executable_path`.
    pub exe_path_regex: Option<Regex>,
    /// Exact match against `Window::executable_path`. Windows paths are
    /// compared case-insensitively, matching the platform's filesystem.
    pub exe_path: Option<String>,
    /// Exact match against `Window::role`.
    pub role: Option<WindowRole>,
}

impl WindowMatcher {
    /// Checks if a window matches all the non-None fields of this matcher.
    pub fn matches(&self, window: &Window) -> bool {
        self.matches_detailed(window).iter().all(|f| f.matched)
    }

    /// Checks if a window matches, returning detailed per-field results.
    pub fn matches_detailed(&self, window: &Window) -> Vec<FieldResult> {
        let mut results = Vec::new();

        if let Some(expected_id) = &self.application_id {
            results.push(FieldResult {
                field: "application_id".to_string(),
                expected: expected_id.clone(),
                actual: window.application_id.0.clone(),
                matched: *expected_id == window.application_id.0,
            });
        }

        if let Some(regex) = &self.application_regex {
            results.push(FieldResult {
                field: "application_regex".to_string(),
                expected: regex.as_str().to_string(),
                actual: window.application_id.0.clone(),
                matched: regex.is_match(&window.application_id.0),
            });
        }

        if let Some(regex) = &self.title_regex {
            results.push(FieldResult {
                field: "title_regex".to_string(),
                expected: regex.as_str().to_string(),
                actual: window.title.clone(),
                matched: regex.is_match(&window.title),
            });
        }

        if let Some(expected_class) = &self.native_class {
            let actual = window.native_class.clone().unwrap_or_default();
            results.push(FieldResult {
                field: "native_class".to_string(),
                expected: expected_class.clone(),
                actual: actual.clone(),
                matched: *expected_class == actual,
            });
        }

        if let Some(regex) = &self.class_regex {
            let actual = window.native_class.clone().unwrap_or_default();
            results.push(FieldResult {
                field: "class_regex".to_string(),
                expected: regex.as_str().to_string(),
                actual: actual.clone(),
                matched: regex.is_match(&actual),
            });
        }

        if let Some(regex) = &self.exe_path_regex {
            let actual = window
                .executable_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            results.push(FieldResult {
                field: "exe_path_regex".to_string(),
                expected: regex.as_str().to_string(),
                actual: actual.clone(),
                matched: regex.is_match(&actual),
            });
        }

        if let Some(expected_path) = &self.exe_path {
            let actual = window
                .executable_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_default();
            let matched = if cfg!(windows) {
                expected_path.eq_ignore_ascii_case(&actual)
            } else {
                expected_path == &actual
            };
            results.push(FieldResult {
                field: "exe_path".to_string(),
                expected: expected_path.clone(),
                actual,
                matched,
            });
        }

        if let Some(expected_role) = &self.role {
            results.push(FieldResult {
                field: "role".to_string(),
                expected: format!("{:?}", expected_role),
                actual: format!("{:?}", window.role),
                matched: *expected_role == window.role,
            });
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaix_domain::{
        ApplicationId, DisplayId, Rect, WindowCapabilities, WindowId, WindowLifecycle,
    };
    use std::path::PathBuf;

    fn create_window() -> Window {
        Window {
            id: WindowId(1),
            process_id: 1234,
            application_id: ApplicationId("com.example.app".to_string()),
            executable_path: Some(PathBuf::from("/usr/bin/example")),
            title: "Example Title".to_string(),
            native_class: Some("ExampleClass".to_string()),
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

    #[test]
    fn test_all_none_matches() {
        let matcher = WindowMatcher {
            application_id: None,
            application_regex: None,
            title_regex: None,
            native_class: None,
            class_regex: None,
            exe_path_regex: None,
            exe_path: None,
            role: None,
        };
        assert!(matcher.matches(&create_window()));
    }

    #[test]
    fn test_exact_application_id() {
        let matcher = WindowMatcher {
            application_id: Some("com.example.app".to_string()),
            application_regex: None,
            title_regex: None,
            native_class: None,
            class_regex: None,
            exe_path_regex: None,
            exe_path: None,
            role: None,
        };
        assert!(matcher.matches(&create_window()));

        let matcher_fail = WindowMatcher {
            application_id: Some("com.other.app".to_string()),
            ..matcher
        };
        assert!(!matcher_fail.matches(&create_window()));
    }

    #[test]
    fn test_regex_fields() {
        let mut matcher = WindowMatcher {
            application_id: None,
            application_regex: Some(Regex::new("example").unwrap()),
            title_regex: Some(Regex::new("^Example").unwrap()),
            native_class: None,
            class_regex: Some(Regex::new("Class$").unwrap()),
            exe_path_regex: Some(Regex::new(r"/example$").unwrap()),
            exe_path: None,
            role: None,
        };
        assert!(matcher.matches(&create_window()));

        matcher.title_regex = Some(Regex::new("Fail").unwrap());
        assert!(!matcher.matches(&create_window()));
    }

    #[test]
    fn test_exact_fields() {
        let matcher = WindowMatcher {
            application_id: None,
            application_regex: None,
            title_regex: None,
            native_class: Some("ExampleClass".to_string()),
            class_regex: None,
            exe_path_regex: None,
            exe_path: None,
            role: Some(WindowRole::Normal),
        };
        assert!(matcher.matches(&create_window()));

        let matcher_fail = WindowMatcher {
            role: Some(WindowRole::Dialog),
            ..matcher
        };
        assert!(!matcher_fail.matches(&create_window()));
    }

    #[test]
    fn test_and_semantics() {
        let matcher = WindowMatcher {
            application_id: Some("com.example.app".to_string()),
            title_regex: Some(Regex::new("Example").unwrap()),
            native_class: None,
            application_regex: None,
            class_regex: None,
            exe_path_regex: None,
            exe_path: None,
            role: None,
        };
        assert!(matcher.matches(&create_window()));

        let matcher_fail = WindowMatcher {
            title_regex: Some(Regex::new("Mismatch").unwrap()),
            ..matcher
        };
        assert!(!matcher_fail.matches(&create_window()));
    }
}
