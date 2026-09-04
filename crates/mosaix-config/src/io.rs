//! Disk I/O and `notify`-based watching for a config directory (ADR 0008).
//!
//! Everything here is the thin I/O shell around [`crate::validate`]'s pure
//! logic: reading `config.toml` and `profiles/*.toml` off disk into a
//! [`CandidateConfig`], writing the first-run default (ADR 0007), and
//! debouncing raw filesystem events into a single validate-and-reload pass
//! (ADR 0008). None of this crate's schema/validation logic lives here --
//! this module only ever hands already-read strings to [`crate::validate`].

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;

use mosaix_domain::NormalizedRect;

use crate::defaults::{default_base_config, default_config_content};
use crate::schema::layout_names_collide;
use crate::schema::{
    AutomaticTilingSection, BaseConfig, Command, ConfigLayer, FocusBorderOverride, GapsOverride,
    KeyCombo, ProfileConfig, ResolvedConfigSet, SavedLayout,
    BASE_CONFIG_FILE_NAME as BASE_FILE_NAME,
};
use crate::validate::{validate, CandidateConfig, CandidateProfile, ValidationError};

/// Debounce window for coalescing raw filesystem events into one
/// validate-and-reload pass (ADR 0008) -- comfortably above how long a
/// small TOML write takes, comfortably below a delay a human editing the
/// file would notice.
pub const DEBOUNCE_WINDOW: Duration = Duration::from_millis(300);

const PROFILES_DIR_NAME: &str = "profiles";

/// Everything that can go wrong reading, creating, or watching a config
/// directory on disk -- distinct from [`ValidationError`], which covers a
/// directory that was read fine but whose *content* is invalid.
#[derive(Debug, Error)]
pub enum ConfigIoError {
    #[error("failed to access {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to start config directory watcher: {source}")]
    Watch {
        #[source]
        source: notify::Error,
    },

    #[error("updated config directory failed validation: {0}")]
    Validation(String),

    #[error(transparent)]
    LayoutEdit(#[from] LayoutEditError),

    #[error(transparent)]
    BindingEdit(#[from] BindingEditError),
}

/// Why a saved-layout edit was never attempted.
///
/// Distinct from [`ConfigIoError::Validation`], which is whole-directory
/// validation's verdict on the candidate an edit produced (ADR 0007).
/// These are refusals reached before any file is written, so the caller
/// can tell "your configuration would be invalid" from "that is not a
/// change I can make".
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LayoutEditError {
    #[error("there is no saved layout named {name:?}")]
    UnknownLayout { name: String },

    #[error("a saved layout named {name:?} already exists")]
    NameTaken { name: String },

    #[error("a saved layout's name may not be empty or whitespace-only")]
    EmptyName,

    #[error(
        "saved layout {name:?} is declared in both {base_file} and {profile_file}; \
         edit those files directly to say which one you mean"
    )]
    DeclaredInBothLayers {
        name: String,
        base_file: String,
        profile_file: String,
    },
}

/// One change to the saved-layout set, performed by the agent on the
/// user's behalf. The settings application never writes a configuration
/// file itself (ADR 0022).
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutEdit {
    /// Create `name`, or replace the cells of the one that exists.
    ///
    /// `to_base` is ADR 0022's redirect: the write normally lands in the
    /// layer that supplies the layout, and this sends it to base config
    /// instead. Redirecting a layout the matched profile declares *moves*
    /// it -- base config gains it and the profile loses it -- because a
    /// copy would leave the profile still winning the merge, so the
    /// redirect would appear to succeed and change nothing at this desk.
    /// It is a no-op for a layout base config already supplies, and for a
    /// layout that does not exist yet, which goes to base config anyway.
    Save {
        name: String,
        cells: Vec<NormalizedRect>,
        to_base: bool,
    },
    Rename {
        from: String,
        to: String,
    },
    /// Copy `from`'s cells to the new layout `to`.
    Duplicate {
        from: String,
        to: String,
    },
    Delete {
        name: String,
    },
}

/// One change to a hotkey binding, performed by the agent on the user's
/// behalf. The settings application never writes a configuration file
/// itself (ADR 0022).
#[derive(Debug, Clone, PartialEq)]
pub enum BindingEdit {
    /// Bind `command` to `combo`.
    ///
    /// `to_base` is ADR 0022's redirect: the write normally lands in the
    /// layer that supplies the binding, and this sends it to base config
    /// instead. Redirecting a binding the matched profile overrides
    /// *moves* it -- base config takes the new combination and the profile
    /// drops its override -- because leaving the override in place would
    /// let the profile keep winning the merge, so a binding the user asked
    /// to apply everywhere would still be desk-specific here.
    Set {
        command: Command,
        combo: KeyCombo,
        to_base: bool,
    },
    /// Step `command` back toward its default.
    ///
    /// Reset removes the binding from the layer that supplies it, and,
    /// when that layer is base config and Mosaix ships a default for the
    /// command, writes the default back. So resetting a profile override
    /// returns the binding to whatever base config says, resetting a base
    /// binding returns it to what a fresh install gives, and a layout
    /// binding -- which has no shipped default -- is simply unbound.
    Reset { command: Command },
}

/// Why a binding edit was never attempted.
///
/// Like [`LayoutEditError`], these are refusals reached before any file is
/// written, so the caller can tell "that is not a change I can make" from
/// whole-directory validation's verdict on the candidate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BindingEditError {
    #[error("nothing is bound to {command}")]
    UnboundCommand { command: String },
}

/// What a binding edit did: which file received it, the combination the
/// binding now resolves to (`None` when the edit left it unbound), and
/// the whole config directory as it now stands.
#[derive(Debug, Clone, PartialEq)]
pub struct BindingWrite {
    pub file: String,
    pub combo: Option<KeyCombo>,
    pub config: ResolvedConfigSet,
}

/// What a layout edit did: which file received it, and the whole config
/// directory as it now stands.
///
/// The resolved set travels back so the agent can make the change live
/// immediately rather than waiting out the reload debounce (ADR 0008) --
/// a user who saves a layout and presses its hotkey should not have to
/// pause first. The echo that arrives through the watcher a moment later
/// is then identical, and the reducer already discards a config change
/// that changes nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutWrite {
    pub file: String,
    pub config: ResolvedConfigSet,
}

#[derive(Debug, Clone)]
pub struct ProfileSettingsUpdate {
    pub fingerprint: String,
    pub automatic_tiling_enabled: bool,
    pub gaps: GapsOverride,
    pub focus_border: FocusBorderOverride,
}

fn io_error(path: &Path, source: std::io::Error) -> ConfigIoError {
    ConfigIoError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Reads `dir`'s `config.toml` and every `.toml` file under `profiles/`
/// into an in-memory [`CandidateConfig`], ready for [`crate::validate`]. A
/// missing `profiles/` subdirectory is treated as zero profiles, not an
/// error -- a fresh single-file config directory is valid.
fn read_candidate(dir: &Path) -> Result<CandidateConfig, ConfigIoError> {
    let base_path = dir.join(BASE_FILE_NAME);
    let base = fs::read_to_string(&base_path).map_err(|err| io_error(&base_path, err))?;

    let mut profiles = Vec::new();
    let profiles_dir = dir.join(PROFILES_DIR_NAME);
    if profiles_dir.is_dir() {
        let entries = fs::read_dir(&profiles_dir).map_err(|err| io_error(&profiles_dir, err))?;
        for entry in entries {
            let entry = entry.map_err(|err| io_error(&profiles_dir, err))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }
            let file_name = path
                .file_name()
                .expect("just read from a directory listing")
                .to_string_lossy()
                .into_owned();
            let contents = fs::read_to_string(&path).map_err(|err| io_error(&path, err))?;
            profiles.push(CandidateProfile {
                file_name,
                contents,
            });
        }
    }

    Ok(CandidateConfig { base, profiles })
}

fn profile_file_name(fingerprint: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    fingerprint.hash(&mut hasher);
    format!("topology-{:016x}.toml", hasher.finish())
}

#[cfg(windows)]
fn replace_file(temp: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|_| std::io::Error::last_os_error())
}

#[cfg(not(windows))]
fn replace_file(temp: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(temp, destination)
}

/// Writes `contents` to `destination` through a temp file and a rename, so
/// a reader never sees a half-written configuration file and a failed
/// write leaves the previous one intact.
///
/// Every configuration write this crate performs goes through here.
fn write_atomically(destination: &Path, contents: &str) -> Result<(), ConfigIoError> {
    let file_name = destination
        .file_name()
        .expect("a configuration destination always names a file")
        .to_string_lossy();
    let temp = destination.with_file_name(format!(".{file_name}.mosaix-tmp"));
    fs::write(&temp, contents).map_err(|error| io_error(&temp, error))?;
    replace_file(&temp, destination).map_err(|error| io_error(destination, error))
}

/// Validates a complete candidate directory before atomically replacing the
/// one topology profile edited by Settings. Existing hotkeys and unrelated
/// profiles are preserved byte-for-byte.
pub fn save_profile_settings(
    dir: &Path,
    update: ProfileSettingsUpdate,
) -> Result<ResolvedConfigSet, ConfigIoError> {
    ensure_default_config(dir)?;
    let mut candidate = read_candidate(dir)?;
    let mut destination_name = None;
    let mut updated_profile = None;

    for profile in &candidate.profiles {
        let parsed: ProfileConfig = toml::from_str(&profile.contents).map_err(|error| {
            ConfigIoError::Validation(format!("{}: {error}", profile.file_name))
        })?;
        if parsed.fingerprint == update.fingerprint {
            destination_name = Some(profile.file_name.clone());
            updated_profile = Some(parsed);
            break;
        }
    }

    let mut profile = updated_profile.unwrap_or(ProfileConfig {
        fingerprint: update.fingerprint.clone(),
        hotkeys: Default::default(),
        gaps: GapsOverride::default(),
        behavior: Default::default(),
        automatic_tiling: None,
        focus_border: FocusBorderOverride::default(),
        layouts: Default::default(),
    });
    // The settings surface toggles activation, not the arrangement, so the
    // profile's existing mode is carried through. Defaulting it here would
    // silently turn a tree profile back into a balanced one on any
    // unrelated save.
    let existing_mode = profile
        .automatic_tiling
        .map(|tiling| tiling.mode)
        .unwrap_or_default();
    profile.automatic_tiling = Some(AutomaticTilingSection {
        enabled: update.automatic_tiling_enabled,
        mode: existing_mode,
    });
    profile.gaps = update.gaps;
    profile.focus_border = update.focus_border;
    let contents = toml::to_string_pretty(&profile)
        .map_err(|error| ConfigIoError::Validation(error.to_string()))?;
    let file_name = destination_name.unwrap_or_else(|| profile_file_name(&update.fingerprint));

    if let Some(candidate_profile) = candidate
        .profiles
        .iter_mut()
        .find(|candidate| candidate.file_name == file_name)
    {
        candidate_profile.contents = contents.clone();
    } else {
        candidate.profiles.push(CandidateProfile {
            file_name: file_name.clone(),
            contents: contents.clone(),
        });
    }
    let resolved = validate(&candidate).map_err(validation_message)?;

    write_atomically(&dir.join(PROFILES_DIR_NAME).join(&file_name), &contents)?;
    Ok(resolved)
}

/// A candidate's base config and its `fingerprint`-matched profile (if
/// any), each paired with the file it was read from -- the two layers a
/// saved-layout edit can land in (ADR 0022).
struct LayoutLayers {
    base: BaseConfig,
    profile: Option<(String, ProfileConfig)>,
}

impl LayoutLayers {
    /// Which layer supplies `name`, `None` if no layer declares it.
    ///
    /// A name both layers declare has no single answer, and guessing would
    /// make a delete look like it failed when base config's copy
    /// reappeared -- so it is reported rather than resolved.
    fn supplier(&self, name: &str) -> Result<Option<ConfigLayer>, LayoutEditError> {
        let in_base = self.base.layouts.contains_key(name);
        let profile = self
            .profile
            .as_ref()
            .filter(|(_, profile)| profile.layouts.contains_key(name));
        match (in_base, profile) {
            (true, Some((file, _))) => Err(LayoutEditError::DeclaredInBothLayers {
                name: name.to_owned(),
                base_file: BASE_FILE_NAME.to_owned(),
                profile_file: file.clone(),
            }),
            (_, Some(_)) => Ok(Some(ConfigLayer::Profile)),
            (true, None) => Ok(Some(ConfigLayer::Base)),
            (false, None) => Ok(None),
        }
    }

    /// Which layer supplies `command`'s binding, `None` if neither binds
    /// it.
    ///
    /// Unlike a saved layout, a command both layers bind is the ordinary
    /// case rather than an ambiguity: that is what a profile override
    /// *is*, and the profile wins the merge, so it is also the layer that
    /// supplies the value on screen.
    fn binding_supplier(&self, command: &Command) -> Option<ConfigLayer> {
        if self
            .profile
            .as_ref()
            .is_some_and(|(_, profile)| profile.hotkeys.contains_key(command))
        {
            return Some(ConfigLayer::Profile);
        }
        self.base
            .hotkeys
            .contains_key(command)
            .then_some(ConfigLayer::Base)
    }

    /// Every layout name either layer declares.
    fn names(&self) -> impl Iterator<Item = &String> {
        self.base.layouts.keys().chain(
            self.profile
                .iter()
                .flat_map(|(_, profile)| profile.layouts.keys()),
        )
    }

    fn cells(&self, name: &str, layer: ConfigLayer) -> Vec<NormalizedRect> {
        let layouts = if layer == ConfigLayer::Profile {
            self.profile
                .as_ref()
                .map(|(_, profile)| &profile.layouts)
                .expect("a profile-supplied layout implies a matched profile")
        } else {
            &self.base.layouts
        };
        layouts
            .get(name)
            .map(|layout| layout.cells.clone())
            .unwrap_or_default()
    }
}

fn parse_layers(
    candidate: &CandidateConfig,
    fingerprint: &str,
) -> Result<LayoutLayers, ConfigIoError> {
    let base: BaseConfig = toml::from_str(&candidate.base)
        .map_err(|error| ConfigIoError::Validation(format!("{BASE_FILE_NAME}: {error}")))?;
    let mut profile = None;
    for candidate_profile in &candidate.profiles {
        let parsed: ProfileConfig =
            toml::from_str(&candidate_profile.contents).map_err(|error| {
                ConfigIoError::Validation(format!("{}: {error}", candidate_profile.file_name))
            })?;
        if parsed.fingerprint == fingerprint {
            profile = Some((candidate_profile.file_name.clone(), parsed));
            break;
        }
    }
    Ok(LayoutLayers { base, profile })
}

/// Rejects `name` as a layout that does not exist yet: empty, or
/// colliding with one that does.
///
/// The collision check ignores case, because whole-directory validation
/// rejects two layouts whose names differ only by case. Catching it here
/// tells the user before the write rather than through a rejected
/// candidate afterwards.
fn check_new_name(layers: &LayoutLayers, name: &str) -> Result<(), LayoutEditError> {
    if name.trim().is_empty() {
        return Err(LayoutEditError::EmptyName);
    }
    if layers
        .names()
        .any(|existing| layout_names_collide(existing, name))
    {
        return Err(LayoutEditError::NameTaken {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// Which file an edit writes and what it ends up saying.
///
/// A rename is a removal and an addition in one file, which is why
/// `removed` and `added` are not two separate decisions. `vacated` is the
/// second file a redirect touches: moving a layout to base config means
/// taking it out of the profile it lived in, and a plan that could only
/// name one file would have to leave it in both.
struct EditPlan {
    /// The layer the layout ends up in, and the one the receipt names.
    destination: ConfigLayer,
    /// The layout to take out of `destination`.
    removed: Option<String>,
    /// The layout to write into `destination`.
    added: Option<(String, Vec<NormalizedRect>)>,
    /// The layout to take out of the matched profile, for a redirect that
    /// moves it to base config (ADR 0022). Always `None` when
    /// `destination` is the profile itself.
    vacated: Option<String>,
}

impl EditPlan {
    /// A plan touching one file, which is every edit but a redirect.
    fn in_place(
        destination: ConfigLayer,
        removed: Option<String>,
        added: Option<(String, Vec<NormalizedRect>)>,
    ) -> Self {
        Self {
            destination,
            removed,
            added,
            vacated: None,
        }
    }
}

fn plan_edit(layers: &LayoutLayers, edit: &LayoutEdit) -> Result<EditPlan, LayoutEditError> {
    let unknown = |name: &String| LayoutEditError::UnknownLayout { name: name.clone() };
    match edit {
        LayoutEdit::Save {
            name,
            cells,
            to_base,
        } => {
            let supplier = layers.supplier(name)?;
            if supplier.is_none() {
                check_new_name(layers, name)?;
            }
            // A redirect only has somewhere to move a layout *from* when
            // the profile is what supplies it. For a base-supplied layout
            // it asks for where the write was already going, and for a
            // new one base config is already the destination.
            let redirecting = *to_base && supplier == Some(ConfigLayer::Profile);
            if redirecting {
                return Ok(EditPlan {
                    destination: ConfigLayer::Base,
                    removed: None,
                    added: Some((name.clone(), cells.clone())),
                    vacated: Some(name.clone()),
                });
            }
            // A layout that exists is rewritten where it lives; a new one
            // goes to base config, so it is available at every desk.
            Ok(EditPlan::in_place(
                supplier.unwrap_or(ConfigLayer::Base),
                None,
                Some((name.clone(), cells.clone())),
            ))
        }
        LayoutEdit::Rename { from, to } => {
            let layer = layers.supplier(from)?.ok_or_else(|| unknown(from))?;
            // Renaming `writing` to `Writing` is a change of case, not a
            // collision with itself: the old name leaves in the same write.
            if !layout_names_collide(from, to) {
                check_new_name(layers, to)?;
            } else if to.trim().is_empty() {
                return Err(LayoutEditError::EmptyName);
            }
            let cells = layers.cells(from, layer);
            Ok(EditPlan::in_place(
                layer,
                Some(from.clone()),
                Some((to.clone(), cells)),
            ))
        }
        LayoutEdit::Duplicate { from, to } => {
            let layer = layers.supplier(from)?.ok_or_else(|| unknown(from))?;
            check_new_name(layers, to)?;
            // The copy lands beside its original, so a variant of a
            // desk-specific layout stays desk-specific.
            let cells = layers.cells(from, layer);
            Ok(EditPlan::in_place(layer, None, Some((to.clone(), cells))))
        }
        LayoutEdit::Delete { name } => {
            let layer = layers.supplier(name)?.ok_or_else(|| unknown(name))?;
            Ok(EditPlan::in_place(layer, Some(name.clone()), None))
        }
    }
}

/// Which file a binding edit writes, and what the binding says there
/// afterwards.
///
/// `vacate_profile` is the override a redirect drops -- the second file
/// such an edit touches, and the binding equivalent of a layout move.
struct BindingPlan {
    destination: ConfigLayer,
    /// What the binding says in `destination` afterwards, `None` when the
    /// edit takes it out.
    bound: Option<KeyCombo>,
    vacate_profile: bool,
}

fn plan_binding_edit(
    layers: &LayoutLayers,
    edit: &BindingEdit,
) -> Result<BindingPlan, BindingEditError> {
    match edit {
        BindingEdit::Set {
            command,
            combo,
            to_base,
        } => {
            let supplier = layers.binding_supplier(command);
            // A redirect only has an override to drop when the profile is
            // what supplies the binding. Otherwise it asks for where the
            // write was already going.
            if *to_base && supplier == Some(ConfigLayer::Profile) {
                return Ok(BindingPlan {
                    destination: ConfigLayer::Base,
                    bound: Some(combo.clone()),
                    vacate_profile: true,
                });
            }
            Ok(BindingPlan {
                destination: supplier.unwrap_or(ConfigLayer::Base),
                bound: Some(combo.clone()),
                vacate_profile: false,
            })
        }
        BindingEdit::Reset { command } => {
            let supplier = layers.binding_supplier(command).ok_or_else(|| {
                BindingEditError::UnboundCommand {
                    command: command.to_string(),
                }
            })?;
            // Resetting a profile override is one step back toward the
            // default: the override goes and base config's value shows
            // through, whatever that is.
            if supplier == ConfigLayer::Profile {
                return Ok(BindingPlan {
                    destination: ConfigLayer::Profile,
                    bound: None,
                    vacate_profile: false,
                });
            }
            Ok(BindingPlan {
                destination: ConfigLayer::Base,
                // A command Mosaix ships a default for goes back to it; a
                // layout binding, which has none, is simply unbound.
                bound: default_base_config().hotkeys.get(command).cloned(),
                vacate_profile: false,
            })
        }
    }
}

/// Applies `edit` to the configuration directory `dir`, writing the layer
/// that currently supplies the binding being edited: the profile matching
/// `fingerprint` if it overrides that command, otherwise base config
/// (ADR 0022). A command neither layer binds yet goes to base config.
///
/// The whole directory is validated as one candidate before anything is
/// written (ADR 0007), so an edit that would leave an invalid
/// configuration -- a duplicate binding, say -- is refused and nothing is
/// persisted. The write itself is a temp file and an atomic replace,
/// matching [`edit_layouts`].
pub fn edit_bindings(
    dir: &Path,
    fingerprint: &str,
    edit: BindingEdit,
) -> Result<BindingWrite, ConfigIoError> {
    ensure_default_config(dir)?;
    let candidate = read_candidate(dir)?;
    let layers = parse_layers(&candidate, fingerprint)?;
    let plan = plan_binding_edit(&layers, &edit)?;
    let command = match &edit {
        BindingEdit::Set { command, .. } | BindingEdit::Reset { command } => command.clone(),
    };
    let into_profile = plan.destination == ConfigLayer::Profile;

    let (file_name, contents) = if into_profile {
        let (file_name, mut profile) = layers
            .profile
            .clone()
            .expect("a profile-supplied binding implies a matched profile");
        apply_binding(&mut profile.hotkeys, &command, &plan.bound);
        (file_name, to_toml(&profile)?)
    } else {
        let mut base = layers.base.clone();
        apply_binding(&mut base.hotkeys, &command, &plan.bound);
        (BASE_FILE_NAME.to_owned(), to_toml(&base)?)
    };

    // The second half of a redirect: the profile drops its override, so
    // base config's new combination is what the merge resolves to.
    let vacated = if plan.vacate_profile {
        let (file_name, mut profile) = layers
            .profile
            .clone()
            .expect("a redirect implies the profile whose override it drops");
        profile.hotkeys.remove(&command);
        Some((file_name, to_toml(&profile)?))
    } else {
        None
    };

    let config = commit(
        dir,
        candidate,
        &PendingWrite {
            into_profile,
            file_name: file_name.clone(),
            contents,
            vacated,
        },
    )?;

    // Read the answer off the resolved set rather than off the plan: what
    // the user now has is the merge result, which a dropped profile
    // override makes base config's value rather than nothing at all.
    let resolved = config
        .profiles
        .iter()
        .find(|profile| profile.fingerprint == fingerprint)
        .map(|profile| &profile.config)
        .unwrap_or(&config.base);
    let combo = resolved.hotkeys.get(&command).cloned();

    Ok(BindingWrite {
        file: file_name,
        combo,
        config,
    })
}

fn apply_binding(
    bindings: &mut std::collections::BTreeMap<Command, KeyCombo>,
    command: &Command,
    bound: &Option<KeyCombo>,
) {
    match bound {
        Some(combo) => {
            bindings.insert(command.clone(), combo.clone());
        }
        None => {
            bindings.remove(command);
        }
    }
}

/// One edit's effect on disk: the file that receives it, what that file
/// now says, and -- for a redirect, which moves a value out of the
/// profile it lived in -- the profile file it also rewrites.
struct PendingWrite {
    /// Whether `file_name` is a profile rather than base config, which is
    /// the difference between the two directories they live in.
    into_profile: bool,
    file_name: String,
    contents: String,
    /// The profile file a redirect empties, and its new contents.
    vacated: Option<(String, String)>,
}

/// Validates the whole directory with `pending` applied and, only if that
/// holds, writes it.
///
/// Whole-directory validation runs before anything is written (ADR 0007),
/// so an edit that would leave the configuration invalid is refused and
/// nothing is persisted. The destination is written before the profile a
/// redirect empties, so a failure between the two leaves the value
/// declared twice -- recoverable by hand -- rather than nowhere.
///
/// Shared by both edit paths because a saved layout and a hotkey binding
/// differ only in what they put in the file, never in how the file gets
/// there.
fn commit(
    dir: &Path,
    mut candidate: CandidateConfig,
    pending: &PendingWrite,
) -> Result<ResolvedConfigSet, ConfigIoError> {
    let patch = |candidate: &mut CandidateConfig, file_name: &str, contents: &str| {
        candidate
            .profiles
            .iter_mut()
            .find(|profile| profile.file_name == file_name)
            .expect("the matched profile was read from this candidate")
            .contents = contents.to_owned();
    };
    if pending.into_profile {
        patch(&mut candidate, &pending.file_name, &pending.contents);
    } else {
        candidate.base = pending.contents.clone();
    }
    if let Some((profile_file, profile_contents)) = &pending.vacated {
        patch(&mut candidate, profile_file, profile_contents);
    }
    let config = validate(&candidate).map_err(validation_message)?;

    let destination = if pending.into_profile {
        dir.join(PROFILES_DIR_NAME).join(&pending.file_name)
    } else {
        dir.join(&pending.file_name)
    };
    write_atomically(&destination, &pending.contents)?;
    if let Some((profile_file, profile_contents)) = &pending.vacated {
        write_atomically(
            &dir.join(PROFILES_DIR_NAME).join(profile_file),
            profile_contents,
        )?;
    }
    Ok(config)
}

/// Applies `edit` to the configuration directory `dir`, writing the layer
/// that currently supplies the layout being edited: the profile matching
/// `fingerprint` if it declares that layout, otherwise base config
/// (ADR 0022). A layout being created belongs to neither layer yet, so it
/// goes to base config.
///
/// The whole directory is validated as one candidate before anything is
/// written (ADR 0007), so an edit that would leave an invalid
/// configuration is refused and nothing is persisted. The write itself is
/// a temp file and an atomic replace, matching [`save_profile_settings`].
pub fn edit_layouts(
    dir: &Path,
    fingerprint: &str,
    edit: LayoutEdit,
) -> Result<LayoutWrite, ConfigIoError> {
    ensure_default_config(dir)?;
    let candidate = read_candidate(dir)?;
    let layers = parse_layers(&candidate, fingerprint)?;
    let plan = plan_edit(&layers, &edit)?;
    let into_profile = plan.destination == ConfigLayer::Profile;

    let (file_name, contents) = if into_profile {
        let (file_name, mut profile) = layers
            .profile
            .clone()
            .expect("a profile-supplied layout implies a matched profile");
        edit_layout_table(&mut profile.layouts, &plan.removed, &plan.added);
        (file_name, to_toml(&profile)?)
    } else {
        let mut base = layers.base.clone();
        edit_layout_table(&mut base.layouts, &plan.removed, &plan.added);
        (BASE_FILE_NAME.to_owned(), to_toml(&base)?)
    };

    // The second half of a redirect: the profile gives the layout up so
    // base config's copy is the one the merge resolves to.
    let vacated = match &plan.vacated {
        Some(name) => {
            let (file_name, mut profile) = layers
                .profile
                .clone()
                .expect("a redirect implies the profile it moves the layout out of");
            profile.layouts.remove(name);
            Some((file_name, to_toml(&profile)?))
        }
        None => None,
    };

    let config = commit(
        dir,
        candidate,
        &PendingWrite {
            into_profile,
            file_name: file_name.clone(),
            contents,
            vacated,
        },
    )?;

    Ok(LayoutWrite {
        file: file_name,
        config,
    })
}

fn edit_layout_table(
    layouts: &mut std::collections::BTreeMap<String, SavedLayout>,
    removed: &Option<String>,
    added: &Option<(String, Vec<NormalizedRect>)>,
) {
    if let Some(name) = removed {
        layouts.remove(name);
    }
    if let Some((name, cells)) = added {
        layouts.insert(
            name.clone(),
            SavedLayout {
                cells: cells.clone(),
            },
        );
    }
}

fn to_toml<T: serde::Serialize>(value: &T) -> Result<String, ConfigIoError> {
    toml::to_string_pretty(value).map_err(|error| ConfigIoError::Validation(error.to_string()))
}

fn validation_message(errors: Vec<ValidationError>) -> ConfigIoError {
    ConfigIoError::Validation(
        errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// Ensures `dir` (and its `profiles/` subdirectory) exist and that
/// `config.toml` is present, writing the generated default content
/// ([`default_config_content`]) if it's missing (ADR 0007's first-run
/// case). A no-op if everything is already there -- never overwrites an
/// existing `config.toml`, valid or not.
pub fn ensure_default_config(dir: &Path) -> Result<(), ConfigIoError> {
    let profiles_dir = dir.join(PROFILES_DIR_NAME);
    fs::create_dir_all(&profiles_dir).map_err(|err| io_error(&profiles_dir, err))?;

    let base_path = dir.join(BASE_FILE_NAME);
    if !base_path.is_file() {
        fs::write(&base_path, default_config_content()).map_err(|err| io_error(&base_path, err))?;
    }

    Ok(())
}

/// Reads and validates `dir` as it stands right now. A directory that
/// can't be read at all surfaces as `Err(ConfigIoError)`; a directory that
/// reads fine but fails validation surfaces as `Ok(Err(errors))` --
/// callers (startup, and [`watch`]'s reload loop) need to tell those two
/// failure modes apart, since only the former has no last-known-good to
/// fall back to at all (ADR 0007).
pub fn load(dir: &Path) -> Result<Result<ResolvedConfigSet, Vec<ValidationError>>, ConfigIoError> {
    let candidate = read_candidate(dir)?;
    Ok(validate(&candidate))
}

/// One outcome of a debounced reload attempt from [`watch`].
#[derive(Debug, Clone)]
pub enum ConfigEvent {
    /// The directory reloaded and validated successfully.
    Changed(ResolvedConfigSet),
    /// The directory reloaded but its content failed validation; the
    /// caller should keep whatever config it already had active. Already
    /// logged by the watcher thread itself when this is produced.
    Rejected(Vec<ValidationError>),
}

/// Internal message type for the watcher thread's own channel -- both the
/// `notify` callback and [`ConfigWatcher::stop`] send into it, exactly the
/// way `mosaix_engine::EngineHandle` uses a `Message::Shutdown` poison pill
/// alongside real events on the same queue.
enum Internal {
    Fs(notify::Result<notify::Event>),
    Stop,
}

/// A running config-directory watcher, owned by a dedicated thread.
/// Dropping (or [`stop`](Self::stop)) tears the watcher down and joins the
/// thread.
pub struct ConfigWatcher {
    tx: Sender<Internal>,
    join_handle: Option<JoinHandle<()>>,
    // Kept alive for the watcher's lifetime -- `notify::Watcher`s stop
    // delivering events as soon as they're dropped.
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// Stops the watcher thread and waits for it to exit.
    pub fn stop(mut self) {
        self.request_stop();
    }

    fn request_stop(&mut self) {
        if let Some(join_handle) = self.join_handle.take() {
            // Best-effort: if the thread already exited (e.g. its
            // `ConfigEvent` receiver was dropped), the channel is
            // disconnected and this returns an error we don't care about.
            let _ = self.tx.send(Internal::Stop);
            let _ = join_handle.join();
        }
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Starts watching `dir` (recursively, so `profiles/` is covered too) for
/// changes, debouncing raw filesystem events ~300ms before reading and
/// validating the whole directory again (ADR 0008).
///
/// Returns a handle controlling the watcher's lifetime and a channel of
/// [`ConfigEvent`]s. Callers are expected to have already run
/// [`ensure_default_config`] (and typically an initial [`load`]) before
/// calling this -- `watch` itself never writes to `dir`.
pub fn watch(dir: PathBuf) -> Result<(ConfigWatcher, Receiver<ConfigEvent>), ConfigIoError> {
    let (internal_tx, internal_rx) = mpsc::channel::<Internal>();
    let (event_tx, event_rx) = mpsc::channel::<ConfigEvent>();

    let notify_tx = internal_tx.clone();
    let mut watcher = notify::recommended_watcher(move |result| {
        let _ = notify_tx.send(Internal::Fs(result));
    })
    .map_err(|source| ConfigIoError::Watch { source })?;

    watcher
        .watch(&dir, RecursiveMode::Recursive)
        .map_err(|source| ConfigIoError::Watch { source })?;

    let join_handle = thread::spawn(move || {
        loop {
            // Block for the first sign of activity (or a stop request).
            match internal_rx.recv() {
                Ok(Internal::Stop) | Err(_) => return,
                Ok(Internal::Fs(Err(err))) => {
                    tracing::warn!(%err, "config directory watch error; ignoring");
                    continue;
                }
                Ok(Internal::Fs(Ok(_))) => {}
            }

            // Debounce: keep draining events until DEBOUNCE passes with no
            // further activity, coalescing a burst of raw events (several
            // `Write`s for one save, or a temp-file-write-then-rename) into
            // a single reload.
            loop {
                match internal_rx.recv_timeout(DEBOUNCE_WINDOW) {
                    Ok(Internal::Stop) => return,
                    Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => break,
                    Ok(Internal::Fs(_)) => continue,
                }
            }

            match load(&dir) {
                Ok(Ok(set)) => {
                    if event_tx.send(ConfigEvent::Changed(set)).is_err() {
                        return;
                    }
                }
                Ok(Err(errors)) => {
                    for error in &errors {
                        tracing::error!(%error, "config reload rejected; keeping last-known-good config");
                    }
                    if event_tx.send(ConfigEvent::Rejected(errors)).is_err() {
                        return;
                    }
                }
                Err(err) => {
                    tracing::error!(
                        %err,
                        "failed to read config directory during reload; keeping last-known-good config"
                    );
                }
            }
        }
    });

    Ok((
        ConfigWatcher {
            tx: internal_tx,
            join_handle: Some(join_handle),
            _watcher: watcher,
        },
        event_rx,
    ))
}
