//! Pure, I/O-free validation: parsing, field-level merge, and the
//! whole-directory checks ADR 0007 requires (wrong version, a duplicate
//! hotkey binding within a resolved config, two profiles sharing a
//! `fingerprint`). Nothing here touches the filesystem or a watcher --
//! callers hand in already-read file contents and get back either a full
//! [`ResolvedConfigSet`] or every error found, never a partial result.

use thiserror::Error;

use crate::schema::{
    BaseConfig, Command, KeyCombo, ProfileConfig, ResolvedConfig, ResolvedConfigSet,
    ResolvedProfile, CURRENT_VERSION,
};

/// One profile candidate: its filename (for error messages -- profiles are
/// matched by content, not filename, per ADR 0004, but the filename is
/// still the natural way to point a user at which file is wrong) and its
/// raw TOML content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateProfile {
    pub file_name: String,
    pub contents: String,
}

/// The whole config directory's contents, already read into memory --
/// `config.toml` plus every file under `profiles/`. The unit
/// [`validate`] validates atomically (ADR 0007).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CandidateConfig {
    pub base: String,
    pub profiles: Vec<CandidateProfile>,
}

/// Everything that can make a candidate config directory invalid. Multiple
/// errors can (and typically will) be reported together for one rejected
/// candidate, since validation never stops at the first problem found.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("{file}: failed to parse TOML: {message}")]
    Parse { file: String, message: String },

    #[error("{file}: unsupported config version {found} (expected {expected})")]
    UnsupportedVersion {
        file: String,
        found: u32,
        expected: u32,
    },

    #[error(
        "{file}: duplicate hotkey binding {combo:?} is bound to both {first:?} and {second:?}"
    )]
    DuplicateBinding {
        file: String,
        combo: String,
        first: Command,
        second: Command,
    },

    #[error(
        "duplicate profile fingerprint {fingerprint:?}: used by both {first_file} and {second_file}"
    )]
    DuplicateFingerprint {
        fingerprint: String,
        first_file: String,
        second_file: String,
    },
}

/// Field-level merges `profile` (if any) over `base`: any field the profile
/// doesn't set falls through to `base`'s value, any field it does set
/// overrides it -- down to individual hotkey commands and individual
/// `Gaps` fields, not whole sections (ADR 0004).
pub fn merge(base: &BaseConfig, profile: Option<&ProfileConfig>) -> ResolvedConfig {
    let mut hotkeys = base.hotkeys.clone();
    let mut gaps = base.gaps;
    let behavior = base.behavior.clone();

    if let Some(profile) = profile {
        for (command, combo) in &profile.hotkeys {
            hotkeys.insert(*command, combo.clone());
        }
        if let Some(outer) = profile.gaps.outer {
            gaps.outer = outer;
        }
        if let Some(inner) = profile.gaps.inner {
            gaps.inner = inner;
        }
    }

    ResolvedConfig {
        hotkeys,
        gaps,
        behavior,
    }
}

fn parse_base(contents: &str) -> Result<BaseConfig, ValidationError> {
    let base: BaseConfig = toml::from_str(contents).map_err(|err| ValidationError::Parse {
        file: "config.toml".to_string(),
        message: err.to_string(),
    })?;
    if base.version != CURRENT_VERSION {
        return Err(ValidationError::UnsupportedVersion {
            file: "config.toml".to_string(),
            found: base.version,
            expected: CURRENT_VERSION,
        });
    }
    Ok(base)
}

fn parse_profile(file_name: &str, contents: &str) -> Result<ProfileConfig, ValidationError> {
    toml::from_str(contents).map_err(|err| ValidationError::Parse {
        file: file_name.to_string(),
        message: err.to_string(),
    })
}

/// Every pair of commands in `resolved` bound to the identical combo, as a
/// validation error naming both. Only the first duplicate found is
/// reported per file -- one collision is enough to reject the candidate,
/// and re-running `validate` after a fix surfaces the next one.
fn duplicate_binding(file: &str, resolved: &ResolvedConfig) -> Option<ValidationError> {
    let entries: Vec<(&Command, &KeyCombo)> = resolved.hotkeys.iter().collect();
    for i in 0..entries.len() {
        for j in (i + 1)..entries.len() {
            if entries[i].1 == entries[j].1 {
                return Some(ValidationError::DuplicateBinding {
                    file: file.to_string(),
                    combo: entries[i].1.to_string(),
                    first: *entries[i].0,
                    second: *entries[j].0,
                });
            }
        }
    }
    None
}

/// Validates a whole candidate config directory atomically (ADR 0007): if
/// any file is invalid -- bad TOML, wrong `version`, a duplicate hotkey
/// binding in a resolved config, or two profiles sharing a `fingerprint`
/// -- the entire candidate is rejected with every error found, and no
/// partial [`ResolvedConfigSet`] is produced.
pub fn validate(candidate: &CandidateConfig) -> Result<ResolvedConfigSet, Vec<ValidationError>> {
    let mut errors = Vec::new();

    let base = match parse_base(&candidate.base) {
        Ok(base) => Some(base),
        Err(err) => {
            errors.push(err);
            None
        }
    };

    let mut profiles: Vec<(&str, ProfileConfig)> = Vec::new();
    for candidate_profile in &candidate.profiles {
        match parse_profile(&candidate_profile.file_name, &candidate_profile.contents) {
            Ok(profile) => profiles.push((&candidate_profile.file_name, profile)),
            Err(err) => errors.push(err),
        }
    }

    for i in 0..profiles.len() {
        for j in (i + 1)..profiles.len() {
            if profiles[i].1.fingerprint == profiles[j].1.fingerprint {
                errors.push(ValidationError::DuplicateFingerprint {
                    fingerprint: profiles[i].1.fingerprint.clone(),
                    first_file: profiles[i].0.to_string(),
                    second_file: profiles[j].0.to_string(),
                });
            }
        }
    }

    let Some(base) = base else {
        return Err(errors);
    };

    let base_resolved = merge(&base, None);
    if let Some(err) = duplicate_binding("config.toml", &base_resolved) {
        errors.push(err);
    }

    let mut resolved_profiles = Vec::new();
    for (file_name, profile) in &profiles {
        let resolved = merge(&base, Some(profile));
        if let Some(err) = duplicate_binding(file_name, &resolved) {
            errors.push(err);
        } else {
            resolved_profiles.push(ResolvedProfile {
                fingerprint: profile.fingerprint.clone(),
                config: resolved,
            });
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(ResolvedConfigSet {
        base: base_resolved,
        profiles: resolved_profiles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::default_config_content;
    use crate::schema::ResolvedConfig;
    use mosaix_domain::Gaps;

    const VALID_BASE: &str = r#"
version = 1

[hotkeys]
snap-left = "ctrl+alt+left"
snap-right = "ctrl+alt+right"
snap-top = "ctrl+alt+up"
snap-bottom = "ctrl+alt+down"

[gaps]
outer = 8
inner = 4

[behavior]
"#;

    fn base_only(base: &str) -> CandidateConfig {
        CandidateConfig {
            base: base.to_string(),
            profiles: Vec::new(),
        }
    }

    fn combo(raw: &str) -> KeyCombo {
        KeyCombo::parse(raw).expect("fixture combo should parse")
    }

    #[test]
    fn valid_single_file_config_parses() {
        let result = validate(&base_only(VALID_BASE)).expect("valid config should be accepted");

        assert_eq!(result.base.gaps, Gaps::new(8, 4));
        assert_eq!(
            result.base.hotkeys.get(&Command::SnapLeft),
            Some(&combo("ctrl+alt+left"))
        );
        assert_eq!(
            result.base.hotkeys.get(&Command::SnapBottom),
            Some(&combo("ctrl+alt+down"))
        );
        assert!(result.profiles.is_empty());
    }

    #[test]
    fn profile_merges_overridden_fields_and_inherits_the_rest() {
        let profile = r#"
fingerprint = "MON-A@0,0 1920x1080 scale=1"

[hotkeys]
snap-left = "ctrl+shift+left"

[gaps]
outer = 20
"#;
        let candidate = CandidateConfig {
            base: VALID_BASE.to_string(),
            profiles: vec![CandidateProfile {
                file_name: "home.toml".to_string(),
                contents: profile.to_string(),
            }],
        };

        let result = validate(&candidate).expect("valid config + profile should be accepted");
        assert_eq!(result.profiles.len(), 1);
        let resolved = &result.profiles[0];
        assert_eq!(resolved.fingerprint, "MON-A@0,0 1920x1080 scale=1");

        // Overridden: the profile's own snap-left and outer gap.
        assert_eq!(
            resolved.config.hotkeys.get(&Command::SnapLeft),
            Some(&combo("ctrl+shift+left"))
        );
        assert_eq!(resolved.config.gaps.outer, 20);

        // Inherited: everything the profile didn't mention.
        assert_eq!(
            resolved.config.hotkeys.get(&Command::SnapRight),
            Some(&combo("ctrl+alt+right"))
        );
        assert_eq!(
            resolved.config.hotkeys.get(&Command::SnapTop),
            Some(&combo("ctrl+alt+up"))
        );
        assert_eq!(
            resolved.config.hotkeys.get(&Command::SnapBottom),
            Some(&combo("ctrl+alt+down"))
        );
        assert_eq!(resolved.config.gaps.inner, 4);

        // Base's own resolved config is untouched by the profile's overrides.
        assert_eq!(result.base.gaps, Gaps::new(8, 4));
    }

    #[test]
    fn invalid_toml_syntax_is_rejected() {
        let errors = validate(&base_only("version = 1\n[hotkeys\n")).unwrap_err();

        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], ValidationError::Parse { .. }));
    }

    #[test]
    fn wrong_version_is_rejected() {
        let bad = VALID_BASE.replace("version = 1", "version = 2");
        let errors = validate(&base_only(&bad)).unwrap_err();

        assert_eq!(
            errors,
            vec![ValidationError::UnsupportedVersion {
                file: "config.toml".to_string(),
                found: 2,
                expected: CURRENT_VERSION,
            }]
        );
    }

    #[test]
    fn missing_version_is_rejected_not_treated_as_current() {
        let bad = r#"
[hotkeys]
snap-left = "ctrl+alt+left"
"#;
        let errors = validate(&base_only(bad)).unwrap_err();

        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], ValidationError::Parse { .. }));
    }

    #[test]
    fn duplicate_binding_is_rejected_naming_both_commands() {
        let bad = r#"
version = 1

[hotkeys]
snap-left = "ctrl+alt+left"
snap-right = "ctrl+alt+left"
"#;
        let errors = validate(&base_only(bad)).unwrap_err();

        assert_eq!(
            errors,
            vec![ValidationError::DuplicateBinding {
                file: "config.toml".to_string(),
                combo: "ctrl+alt+left".to_string(),
                first: Command::SnapLeft,
                second: Command::SnapRight,
            }]
        );
    }

    #[test]
    fn duplicate_fingerprint_across_two_profiles_is_rejected() {
        let profile = |name: &str| CandidateProfile {
            file_name: name.to_string(),
            contents: r#"fingerprint = "MON-A@0,0 1920x1080 scale=1""#.to_string(),
        };
        let candidate = CandidateConfig {
            base: VALID_BASE.to_string(),
            profiles: vec![profile("home.toml"), profile("office.toml")],
        };

        let errors = validate(&candidate).unwrap_err();

        assert_eq!(
            errors,
            vec![ValidationError::DuplicateFingerprint {
                fingerprint: "MON-A@0,0 1920x1080 scale=1".to_string(),
                first_file: "home.toml".to_string(),
                second_file: "office.toml".to_string(),
            }]
        );
    }

    #[test]
    fn default_generated_content_round_trips_through_validate() {
        let result = validate(&base_only(&default_config_content()))
            .expect("the generator's own output must pass validate");

        assert_eq!(result.base, crate::defaults::fallback_config());
    }

    #[test]
    fn merge_with_no_profile_returns_bases_own_settings_unchanged() {
        let base: BaseConfig = toml::from_str(VALID_BASE).unwrap();
        let resolved = merge(&base, None);

        assert_eq!(
            resolved,
            ResolvedConfig {
                hotkeys: base.hotkeys.clone(),
                gaps: base.gaps,
                behavior: base.behavior.clone(),
            }
        );
    }
}
