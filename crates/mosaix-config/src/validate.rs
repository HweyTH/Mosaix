//! Pure, I/O-free validation: parsing, field-level merge, and the
//! whole-directory checks ADR 0007 requires (wrong version, a duplicate
//! hotkey binding within a resolved config, two profiles sharing a
//! `fingerprint`). Nothing here touches the filesystem or a watcher --
//! callers hand in already-read file contents and get back either a full
//! [`ResolvedConfigSet`] or every error found, never a partial result.

use thiserror::Error;

use std::collections::BTreeMap;

use crate::schema::layout_names_collide;
use crate::schema::{
    BaseConfig, Command, ConfigLayer, KeyCombo, ProfileConfig, ResolvedConfig, ResolvedConfigSet,
    ResolvedProfile, SavedLayout, BASE_CONFIG_FILE_NAME, CURRENT_VERSION,
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

    #[error("{file}: duplicate hotkey binding {combo:?} is bound to both {first} and {second}")]
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

    #[error("{file}: focus-border thickness must be between 1 and 16 logical pixels, got {found}")]
    InvalidFocusBorderThickness { file: String, found: u16 },

    #[error("{file}: a saved layout's name may not be empty or whitespace-only")]
    EmptyLayoutName { file: String },

    #[error(
        "{file}: saved layouts {first:?} and {second:?} differ only by case; \
         two layouts may not share a name"
    )]
    DuplicateLayoutName {
        file: String,
        first: String,
        second: String,
    },

    #[error("{file}: saved layout {layout:?} declares no cells")]
    EmptyLayout { file: String, layout: String },

    #[error(
        "{file}: saved layout {layout:?} cell {index} falls outside a display work area; \
         every cell edge must lie within the normalized 0.0-1.0 range"
    )]
    CellOutOfRange {
        file: String,
        layout: String,
        index: usize,
    },

    #[error(
        "{file}: saved layout {layout:?} cell {index} has no width or no height, \
         so it would resize a window to nothing"
    )]
    DegenerateCell {
        file: String,
        layout: String,
        index: usize,
    },

    #[error("{file}: hotkey {binding:?} applies saved layout {layout:?}, which is not declared")]
    UnknownLayoutBinding {
        file: String,
        binding: String,
        layout: String,
    },
}

/// How far past 1.0 a cell edge may land before it counts as outside the
/// work area.
///
/// Cells are written as decimal fractions, and a user splitting a display
/// three ways writes `0.34`/`0.33`/`0.33` -- whose binary sum can exceed
/// 1.0 in the last bits. Rejecting that would be rejecting arithmetic, not
/// a mistake, so the bound is generous enough to swallow rounding and far
/// too tight to admit a real typo.
const NORMALIZED_TOLERANCE: f64 = 1e-9;

/// Every rule a saved layout must satisfy, checked against the file the
/// layouts are *defined* in rather than a merged result -- a name and a
/// cell list belong to whoever wrote them (ADR 0019).
///
/// Returns every violation found rather than the first, matching how
/// [`validate`] reports a whole directory: one pass should show a user
/// everything they need to fix.
fn layout_errors(file: &str, layouts: &BTreeMap<String, SavedLayout>) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    for (name, layout) in layouts {
        if name.trim().is_empty() {
            errors.push(ValidationError::EmptyLayoutName {
                file: file.to_owned(),
            });
        }
        if layout.cells.is_empty() {
            errors.push(ValidationError::EmptyLayout {
                file: file.to_owned(),
                layout: name.clone(),
            });
        }
        for (index, cell) in layout.cells.iter().enumerate() {
            let within =
                |value: f64| (-NORMALIZED_TOLERANCE..=1.0 + NORMALIZED_TOLERANCE).contains(&value);
            if !(within(cell.x)
                && within(cell.y)
                && within(cell.x + cell.width)
                && within(cell.y + cell.height))
            {
                errors.push(ValidationError::CellOutOfRange {
                    file: file.to_owned(),
                    layout: name.clone(),
                    index,
                });
            } else if !(cell.width > 0.0 && cell.height > 0.0) {
                // In range but with nothing in it. Reported separately
                // because "falls outside the work area" would be a lie
                // about a zero-width cell sitting squarely inside it.
                errors.push(ValidationError::DegenerateCell {
                    file: file.to_owned(),
                    layout: name.clone(),
                    index,
                });
            }
        }
    }

    // Exactly-equal names are already impossible -- TOML rejects a repeated
    // key -- so the only collision left to catch is one of case. `writing`
    // and `Writing` are two map entries but one layout to a user, and a
    // binding naming either would be ambiguous.
    let names: Vec<&String> = layouts.keys().collect();
    for i in 0..names.len() {
        for j in (i + 1)..names.len() {
            if layout_names_collide(names[i], names[j]) {
                errors.push(ValidationError::DuplicateLayoutName {
                    file: file.to_owned(),
                    first: names[i].clone(),
                    second: names[j].clone(),
                });
            }
        }
    }

    errors
}

/// Layout names a profile declares that differ only by case from one base
/// config declares, as validation errors against the profile's file.
///
/// An *exactly* equal name is not a collision but the point of a sparse
/// override -- that is how a profile replaces a base layout. A name
/// differing only by case is two map entries but one layout to a user, so
/// a binding naming either would be ambiguous, which is the same reason
/// [`layout_errors`] rejects the collision within a single file.
fn cross_layer_layout_collisions(
    file: &str,
    base: &BTreeMap<String, SavedLayout>,
    profile: &BTreeMap<String, SavedLayout>,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    for profile_name in profile.keys() {
        for base_name in base.keys() {
            if profile_name != base_name && profile_name.to_lowercase() == base_name.to_lowercase()
            {
                errors.push(ValidationError::DuplicateLayoutName {
                    file: file.to_owned(),
                    first: base_name.clone(),
                    second: profile_name.clone(),
                });
            }
        }
    }
    errors
}

/// Field-level merges `profile` (if any) over `base`: any field the profile
/// doesn't set falls through to `base`'s value, any field it does set
/// overrides it -- down to individual hotkey commands and individual
/// `Gaps` fields, not whole sections (ADR 0004).
pub fn merge(base: &BaseConfig, profile: Option<&ProfileConfig>) -> ResolvedConfig {
    let mut hotkeys = base.hotkeys.clone();
    let mut gaps = base.gaps;
    let behavior = base.behavior.clone();
    let mut automatic_tiling_enabled = false;
    let mut focus_border = base.focus_border;
    let mut layouts = base.layouts.clone();
    let mut binding_sources: BTreeMap<Command, ConfigLayer> = base
        .hotkeys
        .keys()
        .map(|command| (command.clone(), ConfigLayer::Base))
        .collect();

    if let Some(profile) = profile {
        for (command, combo) in &profile.hotkeys {
            hotkeys.insert(command.clone(), combo.clone());
            binding_sources.insert(command.clone(), ConfigLayer::Profile);
        }
        if let Some(outer) = profile.gaps.outer {
            gaps.outer = outer;
        }
        if let Some(inner) = profile.gaps.inner {
            gaps.inner = inner;
        }
        automatic_tiling_enabled = profile
            .automatic_tiling
            .is_some_and(|tiling| tiling.enabled);
        if let Some(enabled) = profile.focus_border.enabled {
            focus_border.enabled = enabled;
        }
        if let Some(color) = profile.focus_border.color {
            focus_border.color = color;
        }
        if let Some(thickness) = profile.focus_border.thickness {
            focus_border.thickness = thickness;
        }
        // Keyed by name, so a profile overrides the layouts it names and
        // leaves the rest of base config's set intact (ADR 0004).
        for (name, layout) in &profile.layouts {
            layouts.insert(name.clone(), layout.clone());
        }
    }

    ResolvedConfig {
        hotkeys,
        binding_sources,
        gaps,
        behavior,
        automatic_tiling_enabled,
        focus_border,
        layouts,
        // Attached by `validate`, which is the only place a profile's
        // filename is known.
        profile_file: None,
    }
}

fn parse_base(contents: &str) -> Result<BaseConfig, ValidationError> {
    let base: BaseConfig = toml::from_str(contents).map_err(|err| ValidationError::Parse {
        file: BASE_CONFIG_FILE_NAME.to_string(),
        message: err.to_string(),
    })?;
    if base.version != CURRENT_VERSION {
        return Err(ValidationError::UnsupportedVersion {
            file: BASE_CONFIG_FILE_NAME.to_string(),
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
                    first: entries[i].0.clone(),
                    second: entries[j].0.clone(),
                });
            }
        }
    }
    None
}

/// Every hotkey in `resolved` bound to a saved layout that `resolved` does
/// not declare, as a validation error naming the file, the binding, and
/// the layout (ADR 0019).
///
/// Runs against the merged result rather than one file's own text, because
/// a profile can bind a layout base config declares, or shadow a binding
/// base config made -- what a keypress would actually reach is only
/// visible after the merge.
fn unknown_layout_bindings(file: &str, resolved: &ResolvedConfig) -> Vec<ValidationError> {
    resolved
        .hotkeys
        .iter()
        .filter_map(|(command, combo)| match command {
            Command::ApplyLayout { name } if !resolved.layouts.contains_key(name) => {
                Some(ValidationError::UnknownLayoutBinding {
                    file: file.to_owned(),
                    binding: combo.to_string(),
                    layout: name.clone(),
                })
            }
            _ => None,
        })
        .collect()
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

    errors.extend(layout_errors(BASE_CONFIG_FILE_NAME, &base.layouts));

    let base_resolved = merge(&base, None);
    if !(1..=16).contains(&base_resolved.focus_border.thickness) {
        errors.push(ValidationError::InvalidFocusBorderThickness {
            file: BASE_CONFIG_FILE_NAME.to_owned(),
            found: base_resolved.focus_border.thickness,
        });
    }
    if let Some(err) = duplicate_binding(BASE_CONFIG_FILE_NAME, &base_resolved) {
        errors.push(err);
    }
    errors.extend(unknown_layout_bindings(
        BASE_CONFIG_FILE_NAME,
        &base_resolved,
    ));

    let mut resolved_profiles = Vec::new();
    for (file_name, profile) in &profiles {
        let mut resolved = merge(&base, Some(profile));
        resolved.profile_file = Some((*file_name).to_owned());
        if !(1..=16).contains(&resolved.focus_border.thickness) {
            errors.push(ValidationError::InvalidFocusBorderThickness {
                file: (*file_name).to_owned(),
                found: resolved.focus_border.thickness,
            });
            continue;
        }
        // The profile's own layout declarations are checked against the
        // profile's file, matching how base config's are checked against
        // config.toml -- a name and a cell list belong to whoever wrote
        // them. Only the cross-layer name collision needs both sets.
        let mut layout_problems = layout_errors(file_name, &profile.layouts);
        layout_problems.extend(cross_layer_layout_collisions(
            file_name,
            &base.layouts,
            &profile.layouts,
        ));
        let referential = unknown_layout_bindings(file_name, &resolved);
        let duplicate = duplicate_binding(file_name, &resolved);
        if duplicate.is_some() || !referential.is_empty() || !layout_problems.is_empty() {
            errors.extend(duplicate);
            errors.extend(referential);
            errors.extend(layout_problems);
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
    fn profile_can_override_focus_border_fields_independently() {
        let profile = r#"
fingerprint = "MON-A"

[focus_border]
enabled = false
thickness = 4

[focus_border.color]
red = 240
green = 80
blue = 120
alpha = 200
"#;
        let candidate = CandidateConfig {
            base: VALID_BASE.to_owned(),
            profiles: vec![CandidateProfile {
                file_name: "office.toml".to_owned(),
                contents: profile.to_owned(),
            }],
        };

        let resolved = validate(&candidate).unwrap().profiles.remove(0).config;

        assert!(!resolved.focus_border.enabled);
        assert_eq!(resolved.focus_border.thickness, 4);
        assert_eq!(resolved.focus_border.color.red, 240);
        assert_eq!(resolved.focus_border.color.alpha, 200);
    }

    #[test]
    fn focus_border_thickness_outside_the_supported_range_is_rejected() {
        let bad = format!("{VALID_BASE}\n[focus_border]\nthickness = 0\n");

        let errors = validate(&base_only(&bad)).unwrap_err();

        assert_eq!(
            errors,
            vec![ValidationError::InvalidFocusBorderThickness {
                file: "config.toml".to_owned(),
                found: 0,
            }]
        );
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
                binding_sources: base
                    .hotkeys
                    .keys()
                    .map(|command| (command.clone(), ConfigLayer::Base))
                    .collect(),
                gaps: base.gaps,
                behavior: base.behavior.clone(),
                automatic_tiling_enabled: false,
                focus_border: base.focus_border,
                layouts: base.layouts.clone(),
                profile_file: None,
            }
        );
    }

    #[test]
    fn a_saved_layout_is_declared_as_a_named_list_of_normalized_cells() {
        let base = format!(
            "{VALID_BASE}
             [[layouts.writing.cells]]
             x = 0.0
             y = 0.0
             width = 0.6
             height = 1.0
             
             [[layouts.writing.cells]]
             x = 0.6
             y = 0.0
             width = 0.4
             height = 1.0
"
        );

        let resolved = validate(&base_only(&base))
            .expect("a config declaring a saved layout should be accepted")
            .base;

        let layout = resolved.layouts.get("writing").expect("layout is named");
        assert_eq!(layout.cells.len(), 2);
        assert_eq!(layout.cells[0].width, 0.6);
        assert_eq!(layout.cells[1].x, 0.6);
    }

    #[test]
    fn a_saved_layout_round_trips_through_toml() {
        let base: BaseConfig = toml::from_str(&format!(
            "{VALID_BASE}
[layouts.writing]
cells = [{{ x = 0.0, y = 0.0, width = 0.5, height = 1.0 }}]
"
        ))
        .expect("inline-table cells parse");

        let reparsed: BaseConfig = toml::from_str(&toml::to_string_pretty(&base).unwrap()).unwrap();

        assert_eq!(reparsed, base);
    }

    #[test]
    fn a_misspelled_cell_field_is_rejected_rather_than_silently_defaulted() {
        let base = format!(
            "{VALID_BASE}\n[layouts.writing]\ncells = [{{ x = 0.0, y = 0.0, widht = 0.5, height = 1.0 }}]\n"
        );

        let errors = validate(&base_only(&base)).unwrap_err();

        assert!(
            matches!(errors[0], ValidationError::Parse { ref file, .. } if file == "config.toml"),
            "a typo'd cell field must be reported against the file, got {errors:?}"
        );
    }

    /// A base config declaring the saved layout `writing`, so a binding to
    /// it is referentially sound.
    const BASE_WITH_WRITING: &str = r#"
version = 1

[hotkeys]
snap-left = "ctrl+alt+left"

[layouts.writing]
cells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]

[layouts.coding]
cells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]
"#;

    #[test]
    fn a_binding_to_a_declared_layout_is_accepted() {
        let base =
            format!("{BASE_WITH_WRITING}\n[hotkeys.apply-layout]\nwriting = \"ctrl+alt+1\"\n");

        let resolved = validate(&base_only(&base))
            .expect("a binding naming a declared layout is sound")
            .base;

        assert_eq!(
            resolved.hotkeys.get(&Command::ApplyLayout {
                name: "writing".to_owned()
            }),
            Some(&combo("ctrl+alt+1"))
        );
    }

    #[test]
    fn a_binding_naming_a_layout_that_does_not_exist_is_rejected() {
        let base =
            format!("{BASE_WITH_WRITING}\n[hotkeys.apply-layout]\nwrtiing = \"ctrl+alt+1\"\n");

        let errors = validate(&base_only(&base)).unwrap_err();

        assert!(
            errors.contains(&ValidationError::UnknownLayoutBinding {
                file: "config.toml".to_owned(),
                binding: "ctrl+alt+1".to_owned(),
                layout: "wrtiing".to_owned(),
            }),
            "got {errors:?}"
        );
        // File, binding, and missing layout all in the one message the
        // user sees (ADR 0019).
        let message = errors[0].to_string();
        assert!(message.contains("config.toml"), "{message}");
        assert!(message.contains("ctrl+alt+1"), "{message}");
        assert!(message.contains("wrtiing"), "{message}");
    }

    #[test]
    fn a_profiles_binding_to_a_layout_that_does_not_exist_is_rejected_naming_the_profile() {
        let candidate = CandidateConfig {
            base: BASE_WITH_WRITING.to_owned(),
            profiles: vec![CandidateProfile {
                file_name: "office.toml".to_owned(),
                contents:
                    "fingerprint = \"MON-A\"\n[hotkeys.apply-layout]\nwrtiing = \"ctrl+alt+1\"\n"
                        .to_owned(),
            }],
        };

        let errors = validate(&candidate).unwrap_err();

        assert!(
            errors.contains(&ValidationError::UnknownLayoutBinding {
                file: "office.toml".to_owned(),
                binding: "ctrl+alt+1".to_owned(),
                layout: "wrtiing".to_owned(),
            }),
            "got {errors:?}"
        );
    }

    #[test]
    fn a_layout_binding_is_checked_against_the_merged_layout_set() {
        // Base declares the layout; only the profile binds it. Checking
        // either file alone would miss that this is sound.
        let candidate = CandidateConfig {
            base: BASE_WITH_WRITING.to_owned(),
            profiles: vec![CandidateProfile {
                file_name: "office.toml".to_owned(),
                contents:
                    "fingerprint = \"MON-A\"\n[hotkeys.apply-layout]\nwriting = \"ctrl+alt+1\"\n"
                        .to_owned(),
            }],
        };

        let result = validate(&candidate).expect("base declares what the profile binds");

        assert!(result.profiles[0]
            .config
            .hotkeys
            .contains_key(&Command::ApplyLayout {
                name: "writing".to_owned()
            }));
    }

    #[test]
    fn bindings_of_two_different_layouts_are_not_a_duplicate() {
        let base = format!(
            "{BASE_WITH_WRITING}\n\
             [hotkeys.apply-layout]\n\
             writing = \"ctrl+alt+1\"\n\
             coding = \"ctrl+alt+2\"\n"
        );

        let resolved = validate(&base_only(&base))
            .expect("two layouts on two combos is two bindings, not a collision")
            .base;

        assert_eq!(resolved.hotkeys.len(), 3);
    }

    #[test]
    fn a_profile_rebinding_a_layout_is_an_override_not_a_duplicate() {
        // Base and a profile both binding `apply-layout.writing` is the
        // ordinary field-level merge every other binding gets (ADR 0004),
        // not a collision -- the profile's combo simply wins for its
        // topology, exactly as it would for `snap-left`.
        let candidate = CandidateConfig {
            base: format!(
                "{BASE_WITH_WRITING}\n[hotkeys.apply-layout]\nwriting = \"ctrl+alt+1\"\n"
            ),
            profiles: vec![CandidateProfile {
                file_name: "office.toml".to_owned(),
                contents:
                    "fingerprint = \"MON-A\"\n[hotkeys.apply-layout]\nwriting = \"ctrl+alt+2\"\n"
                        .to_owned(),
            }],
        };

        let result = validate(&candidate).expect("a profile override is not a duplicate");

        let binding = Command::ApplyLayout {
            name: "writing".to_owned(),
        };
        assert_eq!(
            result.base.hotkeys.get(&binding),
            Some(&combo("ctrl+alt+1"))
        );
        assert_eq!(
            result.profiles[0].config.hotkeys.get(&binding),
            Some(&combo("ctrl+alt+2")),
            "the profile's combo wins for its own topology"
        );
    }

    #[test]
    fn binding_the_same_layout_to_two_combos_in_one_file_is_rejected_where_it_is_written() {
        // One layout is one key of one map, so two combos for it cannot
        // both survive. TOML catches the repeated key first, which is the
        // earliest and most precise place to catch it -- the alternative
        // is a silent last-wins, which ADR 0019 rules out.
        let base = format!(
            "{BASE_WITH_WRITING}\n\
             [hotkeys.apply-layout]\n\
             writing = \"ctrl+alt+1\"\n\
             writing = \"ctrl+alt+2\"\n"
        );

        let errors = validate(&base_only(&base)).unwrap_err();

        assert!(
            matches!(errors[0], ValidationError::Parse { ref file, .. } if file == "config.toml"),
            "got {errors:?}"
        );
    }

    #[test]
    fn a_layout_binding_colliding_with_a_verb_is_a_duplicate_naming_both() {
        let base =
            format!("{BASE_WITH_WRITING}\n[hotkeys.apply-layout]\nwriting = \"ctrl+alt+left\"\n");

        let errors = validate(&base_only(&base)).unwrap_err();

        assert_eq!(
            errors,
            vec![ValidationError::DuplicateBinding {
                file: "config.toml".to_owned(),
                combo: "ctrl+alt+left".to_owned(),
                first: Command::SnapLeft,
                second: Command::ApplyLayout {
                    name: "writing".to_owned()
                },
            }]
        );
        assert!(
            errors[0].to_string().contains("apply-layout.writing"),
            "the payload must appear in the message, got {}",
            errors[0]
        );
    }

    #[test]
    fn duplicate_detection_still_runs_after_base_and_profile_are_merged() {
        // Neither file collides on its own: base binds snap-left, the
        // profile binds a layout, and only the merged set has both on
        // ctrl+alt+1.
        let candidate = CandidateConfig {
            base: BASE_WITH_WRITING.replace(
                "snap-left = \"ctrl+alt+left\"",
                "snap-left = \"ctrl+alt+1\"",
            ),
            profiles: vec![CandidateProfile {
                file_name: "office.toml".to_owned(),
                contents:
                    "fingerprint = \"MON-A\"\n[hotkeys.apply-layout]\nwriting = \"ctrl+alt+1\"\n"
                        .to_owned(),
            }],
        };

        let errors = validate(&candidate).unwrap_err();

        assert!(
            errors.iter().any(|error| matches!(
                error,
                ValidationError::DuplicateBinding { file, .. } if file == "office.toml"
            )),
            "got {errors:?}"
        );
    }

    /// `VALID_BASE` with `layouts` appended -- every layout rule test
    /// differs only in what it declares there.
    fn base_with_layouts(layouts: &str) -> CandidateConfig {
        base_only(&format!("{VALID_BASE}\n{layouts}"))
    }

    #[test]
    fn an_empty_layout_name_is_rejected_naming_the_file() {
        let errors = validate(&base_with_layouts(
            "[layouts.\"\"]\ncells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]\n",
        ))
        .unwrap_err();

        assert!(
            errors.contains(&ValidationError::EmptyLayoutName {
                file: "config.toml".to_owned(),
            }),
            "got {errors:?}"
        );
        assert!(errors[0].to_string().contains("config.toml"));
    }

    #[test]
    fn a_whitespace_only_layout_name_is_rejected() {
        let errors = validate(&base_with_layouts(
            "[layouts.\"   \"]\ncells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]\n",
        ))
        .unwrap_err();

        assert!(
            errors.contains(&ValidationError::EmptyLayoutName {
                file: "config.toml".to_owned(),
            }),
            "got {errors:?}"
        );
    }

    #[test]
    fn two_layout_names_differing_only_by_case_are_rejected() {
        let errors = validate(&base_with_layouts(
            "[layouts.writing]\ncells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]\n\
             [layouts.Writing]\ncells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]\n",
        ))
        .unwrap_err();

        assert!(
            errors.contains(&ValidationError::DuplicateLayoutName {
                file: "config.toml".to_owned(),
                first: "Writing".to_owned(),
                second: "writing".to_owned(),
            }),
            "got {errors:?}"
        );
    }

    #[test]
    fn two_layouts_with_genuinely_different_names_are_accepted() {
        let resolved = validate(&base_with_layouts(
            "[layouts.writing]\ncells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]\n\
             [layouts.coding]\ncells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]\n",
        ))
        .expect("two distinct names are not a collision")
        .base;

        assert_eq!(resolved.layouts.len(), 2);
    }

    #[test]
    fn a_layout_with_an_empty_cell_list_is_rejected() {
        let errors = validate(&base_with_layouts("[layouts.writing]\ncells = []\n")).unwrap_err();

        assert!(
            errors.contains(&ValidationError::EmptyLayout {
                file: "config.toml".to_owned(),
                layout: "writing".to_owned(),
            }),
            "got {errors:?}"
        );
    }

    #[test]
    fn a_layout_declaring_no_cells_at_all_is_rejected_the_same_way() {
        // `cells` defaults to empty rather than failing to parse, so an
        // omitted list must reach the same rule as an explicitly empty one.
        let errors = validate(&base_with_layouts("[layouts.writing]\n")).unwrap_err();

        assert!(
            errors.contains(&ValidationError::EmptyLayout {
                file: "config.toml".to_owned(),
                layout: "writing".to_owned(),
            }),
            "got {errors:?}"
        );
    }

    #[test]
    fn a_cell_running_past_the_right_edge_of_the_work_area_is_rejected() {
        let errors = validate(&base_with_layouts(
            "[layouts.writing]\ncells = [{ x = 0.6, y = 0.0, width = 0.5, height = 1.0 }]\n",
        ))
        .unwrap_err();

        assert!(
            errors.contains(&ValidationError::CellOutOfRange {
                file: "config.toml".to_owned(),
                layout: "writing".to_owned(),
                index: 0,
            }),
            "got {errors:?}"
        );
    }

    #[test]
    fn a_negative_cell_origin_is_rejected() {
        let errors = validate(&base_with_layouts(
            "[layouts.writing]\ncells = [{ x = -0.1, y = 0.0, width = 0.5, height = 1.0 }]\n",
        ))
        .unwrap_err();

        assert!(
            errors.contains(&ValidationError::CellOutOfRange {
                file: "config.toml".to_owned(),
                layout: "writing".to_owned(),
                index: 0,
            }),
            "got {errors:?}"
        );
    }

    #[test]
    fn a_zero_width_cell_is_rejected_as_degenerate_not_as_out_of_range() {
        let errors = validate(&base_with_layouts(
            "[layouts.writing]\ncells = [{ x = 0.5, y = 0.0, width = 0.0, height = 1.0 }]\n",
        ))
        .unwrap_err();

        assert!(
            errors.contains(&ValidationError::DegenerateCell {
                file: "config.toml".to_owned(),
                layout: "writing".to_owned(),
                index: 0,
            }),
            "got {errors:?}"
        );
        // The cell sits squarely inside the work area, so the
        // out-of-range wording would be a lie about it.
        assert!(
            !errors[0].to_string().contains("outside"),
            "got {}",
            errors[0]
        );
    }

    #[test]
    fn the_offending_cell_is_named_by_its_position_in_the_list() {
        let errors = validate(&base_with_layouts(
            "[layouts.writing]\ncells = [\n\
               { x = 0.0, y = 0.0, width = 0.5, height = 1.0 },\n\
               { x = 0.5, y = 0.0, width = 0.5, height = 1.0 },\n\
               { x = 0.5, y = 0.0, width = 0.9, height = 1.0 },\n\
             ]\n",
        ))
        .unwrap_err();

        assert!(
            errors.contains(&ValidationError::CellOutOfRange {
                file: "config.toml".to_owned(),
                layout: "writing".to_owned(),
                index: 2,
            }),
            "got {errors:?}"
        );
    }

    #[test]
    fn a_three_way_split_written_as_decimals_is_not_rejected_for_binary_rounding() {
        // 0.34 + 0.33 + 0.33 sums past 1.0 in binary. That is arithmetic,
        // not a user error, and must survive the range check.
        validate(&base_with_layouts(
            "[layouts.thirds]\ncells = [\n\
               { x = 0.0, y = 0.0, width = 0.34, height = 1.0 },\n\
               { x = 0.34, y = 0.0, width = 0.33, height = 1.0 },\n\
               { x = 0.67, y = 0.0, width = 0.33, height = 1.0 },\n\
             ]\n",
        ))
        .expect("a decimal three-way split is a valid layout");
    }

    #[test]
    fn a_layout_covering_the_whole_work_area_is_accepted_at_the_boundary() {
        validate(&base_with_layouts(
            "[layouts.full]\ncells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]\n",
        ))
        .expect("edges exactly on the boundary are inside it");
    }

    #[test]
    fn every_layout_problem_in_one_file_is_reported_together() {
        let errors = validate(&base_with_layouts(
            "[layouts.writing]\ncells = []\n\
             [layouts.coding]\ncells = [{ x = 0.0, y = 0.0, width = 2.0, height = 1.0 }]\n",
        ))
        .unwrap_err();

        assert_eq!(
            errors.len(),
            2,
            "one pass should show the user everything, got {errors:?}"
        );
    }

    #[test]
    fn a_config_declaring_no_layouts_resolves_to_an_empty_set() {
        let resolved = validate(&base_only(VALID_BASE)).unwrap().base;

        assert!(resolved.layouts.is_empty());
    }

    /// Base config declaring two layouts, `writing` covering the whole work
    /// area and `coding` splitting it in half, so a profile overriding one
    /// of them is visibly distinguishable from a profile overriding both.
    const BASE_WITH_TWO_LAYOUTS: &str = r#"
version = 1

[layouts.writing]
cells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]

[layouts.coding]
cells = [
  { x = 0.0, y = 0.0, width = 0.5, height = 1.0 },
  { x = 0.5, y = 0.0, width = 0.5, height = 1.0 },
]
"#;

    fn with_profile(base: &str, contents: &str) -> CandidateConfig {
        CandidateConfig {
            base: base.to_owned(),
            profiles: vec![CandidateProfile {
                file_name: "desk.toml".to_owned(),
                contents: contents.to_owned(),
            }],
        }
    }

    /// The one profile `validate` resolved for `candidate`.
    fn only_profile(candidate: &CandidateConfig) -> ResolvedConfig {
        let mut set = validate(candidate).expect("candidate should validate");
        assert_eq!(
            set.profiles.len(),
            1,
            "fixture declares exactly one profile"
        );
        set.profiles.remove(0).config
    }

    #[test]
    fn a_profile_layout_override_replaces_only_the_layout_it_names() {
        let resolved = only_profile(&with_profile(
            BASE_WITH_TWO_LAYOUTS,
            "fingerprint = \"DESK\"\n\
             [layouts.writing]\n\
             cells = [{ x = 0.25, y = 0.0, width = 0.5, height = 1.0 }]\n",
        ));

        let writing = resolved.layouts.get("writing").expect("still declared");
        assert_eq!(writing.cells.len(), 1);
        assert_eq!(writing.cells[0].x, 0.25, "the profile's cells win");
        assert_eq!(
            resolved.layouts.get("coding"),
            validate(&base_only(BASE_WITH_TWO_LAYOUTS))
                .unwrap()
                .base
                .layouts
                .get("coding"),
            "a layout the profile does not mention falls through unchanged"
        );
    }

    #[test]
    fn a_profile_can_add_a_layout_base_config_does_not_have() {
        let resolved = only_profile(&with_profile(
            BASE_WITH_TWO_LAYOUTS,
            "fingerprint = \"DESK\"\n\
             [layouts.docked]\n\
             cells = [{ x = 0.0, y = 0.0, width = 0.34, height = 1.0 }]\n",
        ));

        assert!(resolved.layouts.contains_key("docked"));
        assert!(
            resolved.layouts.contains_key("writing") && resolved.layouts.contains_key("coding"),
            "adding a layout does not replace the base set, got {:?}",
            resolved.layouts.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_topology_matching_no_profile_resolves_to_the_base_layouts() {
        let candidate = with_profile(
            BASE_WITH_TWO_LAYOUTS,
            "fingerprint = \"DESK\"\n\
             [layouts.docked]\n\
             cells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]\n",
        );

        // `validate` resolves base on its own as well as each profile, and
        // base is what the engine selects when no fingerprint matches.
        let base = validate(&candidate).unwrap().base;

        assert_eq!(
            base.layouts.keys().collect::<Vec<_>>(),
            vec!["coding", "writing"],
            "the base resolution must not see the profile's addition"
        );
    }

    #[test]
    fn a_profile_declaring_no_layouts_inherits_the_whole_base_set() {
        let resolved = only_profile(&with_profile(
            BASE_WITH_TWO_LAYOUTS,
            "fingerprint = \"DESK\"\n[gaps]\nouter = 12\n",
        ));

        assert_eq!(
            resolved.layouts.keys().collect::<Vec<_>>(),
            vec!["coding", "writing"]
        );
    }

    #[test]
    fn a_profiles_own_layout_rules_are_checked_against_the_profile_file() {
        let errors = validate(&with_profile(
            BASE_WITH_TWO_LAYOUTS,
            "fingerprint = \"DESK\"\n[layouts.docked]\ncells = []\n",
        ))
        .unwrap_err();

        assert!(
            errors.contains(&ValidationError::EmptyLayout {
                file: "desk.toml".to_owned(),
                layout: "docked".to_owned(),
            }),
            "the profile's own file must be named, got {errors:?}"
        );
    }

    #[test]
    fn a_profile_layout_differing_only_by_case_from_a_base_layout_is_rejected() {
        let errors = validate(&with_profile(
            BASE_WITH_TWO_LAYOUTS,
            "fingerprint = \"DESK\"\n\
             [layouts.Writing]\n\
             cells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]\n",
        ))
        .unwrap_err();

        assert!(
            errors.contains(&ValidationError::DuplicateLayoutName {
                file: "desk.toml".to_owned(),
                first: "writing".to_owned(),
                second: "Writing".to_owned(),
            }),
            "got {errors:?}"
        );
    }

    #[test]
    fn a_profile_can_bind_a_layout_only_it_declares() {
        let resolved = only_profile(&with_profile(
            BASE_WITH_TWO_LAYOUTS,
            "fingerprint = \"DESK\"\n\
             [hotkeys.apply-layout]\n\
             docked = \"ctrl+alt+1\"\n\
             [layouts.docked]\n\
             cells = [{ x = 0.0, y = 0.0, width = 1.0, height = 1.0 }]\n",
        ));

        assert_eq!(
            resolved.hotkeys.get(&Command::ApplyLayout {
                name: "docked".to_owned()
            }),
            Some(&combo("ctrl+alt+1")),
            "the referential check runs against the merged layout set"
        );
    }

    #[test]
    fn a_profile_layouts_table_survives_a_round_trip_through_toml() {
        let profile: ProfileConfig = toml::from_str(
            "fingerprint = \"DESK\"\n\
             [layouts.docked]\n\
             cells = [{ x = 0.0, y = 0.0, width = 0.5, height = 1.0 }]\n",
        )
        .expect("a profile layouts table parses");

        let rendered = toml::to_string_pretty(&profile).unwrap();
        let reparsed: ProfileConfig = toml::from_str(&rendered).unwrap();

        assert_eq!(reparsed, profile);
    }

    /// Base config binding two commands, so a profile overriding one of
    /// them leaves the other visibly base-supplied.
    const BASE_WITH_TWO_BINDINGS: &str = r#"
version = 1

[hotkeys]
snap-left = "ctrl+alt+left"
snap-right = "ctrl+alt+right"
"#;

    #[test]
    fn a_binding_no_profile_touches_is_recorded_as_base_supplied() {
        let resolved = validate(&base_only(BASE_WITH_TWO_BINDINGS)).unwrap().base;

        assert_eq!(
            resolved.binding_sources.get(&Command::SnapLeft),
            Some(&ConfigLayer::Base)
        );
        assert_eq!(
            resolved.profile_file, None,
            "base config alone supplies it, so there is no profile file to name"
        );
    }

    #[test]
    fn a_binding_the_matched_profile_overrides_is_recorded_as_profile_supplied() {
        let resolved = only_profile(&with_profile(
            BASE_WITH_TWO_BINDINGS,
            "fingerprint = \"DESK\"\n[hotkeys]\nsnap-left = \"ctrl+shift+left\"\n",
        ));

        assert_eq!(
            resolved.binding_sources.get(&Command::SnapLeft),
            Some(&ConfigLayer::Profile),
            "the profile supplies the value on screen, so it receives the write"
        );
        assert_eq!(
            resolved.binding_sources.get(&Command::SnapRight),
            Some(&ConfigLayer::Base),
            "a binding the profile does not mention still comes from base config"
        );
        assert_eq!(
            resolved.profile_file,
            Some("desk.toml".to_owned()),
            "the file a profile-supplied write would land in has to be nameable"
        );
    }

    #[test]
    fn a_binding_only_the_profile_declares_is_recorded_as_profile_supplied() {
        let resolved = only_profile(&with_profile(
            BASE_WITH_TWO_BINDINGS,
            "fingerprint = \"DESK\"\n[hotkeys]\ntoggle-pause = \"ctrl+alt+p\"\n",
        ));

        assert_eq!(
            resolved.binding_sources.get(&Command::TogglePause),
            Some(&ConfigLayer::Profile)
        );
    }

    #[test]
    fn a_layout_binding_carries_provenance_like_any_other() {
        let resolved = only_profile(&with_profile(
            BASE_WITH_TWO_LAYOUTS,
            "fingerprint = \"DESK\"\n[hotkeys.apply-layout]\nwriting = \"ctrl+alt+1\"\n",
        ));

        assert_eq!(
            resolved.binding_sources.get(&Command::ApplyLayout {
                name: "writing".to_owned()
            }),
            Some(&ConfigLayer::Profile)
        );
    }

    #[test]
    fn every_resolved_binding_has_a_source_and_no_source_lacks_a_binding() {
        let resolved = only_profile(&with_profile(
            BASE_WITH_TWO_BINDINGS,
            "fingerprint = \"DESK\"\n[hotkeys]\nsnap-left = \"ctrl+shift+left\"\ntoggle-pause = \"ctrl+alt+p\"\n",
        ));

        assert_eq!(
            resolved.hotkeys.keys().collect::<Vec<_>>(),
            resolved.binding_sources.keys().collect::<Vec<_>>(),
            "the parallel field must stay keyed identically to the map it describes"
        );
    }

    #[test]
    fn a_profile_with_no_layouts_grows_no_empty_layouts_table() {
        let profile: ProfileConfig = toml::from_str("fingerprint = \"DESK\"\n").unwrap();

        let rendered = toml::to_string_pretty(&profile).unwrap();

        assert!(
            !rendered.contains("[layouts]"),
            "rewriting a layout-free profile must not add a header, got:\n{rendered}"
        );
    }
}
