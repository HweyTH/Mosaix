//! Logical workspaces: one global pool of uniquely named managed-window
//! groups.
//!
//! The pool is pure state with typed transitions. It knows which workspace
//! is displayed on which display, which workspace each managed window
//! belongs to, and which window each workspace last had focused. It does
//! not know how a window is placed or parked: the reducer asks the pool
//! what a command would change and performs the placements itself.
//!
//! Every way a lifecycle command can change nothing is a
//! [`WorkspaceRefusal`], so a caller is told why rather than hearing
//! nothing back. Nothing here ever creates a workspace as a side effect of
//! naming one: a focus, move, or rule target that names an unknown
//! workspace is refused.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::id::{DisplayId, WindowId};
use crate::recovery::ParkingRefusal;
use crate::tree::{ContainerTree, PersistedTree};

/// The longest name a workspace may have, in characters. Long enough for
/// any sensible label, short enough that a pasted paragraph is refused.
pub const MAX_WORKSPACE_NAME_CHARS: usize = 64;

/// Why a string cannot name a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceNameError {
    Empty,
    TooLong {
        chars: usize,
    },
    /// A control character, or a character that would make the name
    /// unusable on a command line or in a TOML key.
    ForbiddenCharacter {
        character: char,
    },
}

impl std::fmt::Display for WorkspaceNameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("a workspace name may not be empty"),
            Self::TooLong { chars } => write!(
                formatter,
                "a workspace name may be at most {MAX_WORKSPACE_NAME_CHARS} characters, got {chars}"
            ),
            Self::ForbiddenCharacter { character } => {
                write!(formatter, "a workspace name may not contain {character:?}")
            }
        }
    }
}

/// A validated workspace name. Two names that differ only by case are the
/// same workspace, the way two saved layouts are (the pool compares them
/// through [`WorkspaceName::key`]); the name is stored as written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceName(String);

impl WorkspaceName {
    pub fn new(raw: &str) -> Result<Self, WorkspaceNameError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(WorkspaceNameError::Empty);
        }
        let chars = trimmed.chars().count();
        if chars > MAX_WORKSPACE_NAME_CHARS {
            return Err(WorkspaceNameError::TooLong { chars });
        }
        if let Some(character) = trimmed
            .chars()
            .find(|character| character.is_control() || matches!(character, '/' | '\\' | '"'))
        {
            return Err(WorkspaceNameError::ForbiddenCharacter { character });
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The case-folded form two names are compared by.
    pub fn key(&self) -> String {
        self.0.to_lowercase()
    }

    /// Whether two names name the same workspace.
    pub fn collides_with(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl std::fmt::Display for WorkspaceName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// How a workspace came to exist. A configuration-declared workspace is
/// owned by the file that declares it and cannot be deleted by command;
/// a command-created one persists in the state database until deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WorkspaceOrigin {
    Configuration,
    #[default]
    Command,
}

impl WorkspaceOrigin {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Command => "command",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "configuration" => Some(Self::Configuration),
            "command" => Some(Self::Command),
            _ => None,
        }
    }
}

/// One workspace as the pool holds it.
///
/// The container tree follows the workspace: while the workspace is
/// displayed, its tree is the live tree the reducer reflows for that
/// display, and while it is hidden the tree waits here in `stashed_tree`
/// with its dormant leaves intact. That is what makes a move between
/// monitors preserve structure, and what makes deletion refuse while a
/// dormant leaf still holds a position.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Workspace {
    pub origin: WorkspaceOrigin,
    /// The tree a hidden workspace keeps. `None` while displayed, because
    /// then the reducer's per-display tree is the authority.
    pub stashed_tree: Option<ContainerTree>,
    /// The window that last had focus while a member of this workspace,
    /// so returning to the workspace can restore the point of attention.
    pub last_focused: Option<WindowId>,
}

/// The durable form of one workspace: what survives a restart.
///
/// Native window ids do not, so membership and last focus are absent; the
/// tree carries the same evidence a stored container tree does and is
/// matched the same way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedWorkspace {
    pub name: WorkspaceName,
    pub origin: WorkspaceOrigin,
    /// The stable fingerprint of the display this workspace was displayed
    /// on when last written, or `None` when it was hidden.
    pub displayed_fingerprint: Option<String>,
    pub tree: Option<PersistedTree>,
}

/// Why a workspace lifecycle command changed nothing. Every variant names
/// something the user can act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceRefusal {
    /// The name is not a valid workspace name at all.
    InvalidName { reason: WorkspaceNameError },
    /// No workspace by that name exists, and naming one does not create
    /// it.
    UnknownWorkspace { name: String },
    /// Create was asked for a name the pool already holds.
    AlreadyExists { name: WorkspaceName },
    /// Delete was asked for a workspace that is currently displayed.
    Displayed {
        name: WorkspaceName,
        display_id: DisplayId,
    },
    /// Delete was asked for a workspace that still owns positions.
    NotEmpty {
        name: WorkspaceName,
        live_members: usize,
        dormant_positions: usize,
    },
    /// Delete was asked for a workspace a configuration file declares.
    /// Removing the declaration is the way to delete it.
    DeclaredByConfiguration { name: WorkspaceName },
    /// Move was asked for a hidden workspace; focus is the command that
    /// displays one.
    NotDisplayed { name: WorkspaceName },
    /// Focus needed a display to show a hidden workspace on, and no
    /// display is focused.
    NoFocusedDisplay,
    /// The named display is not in the current topology.
    UnknownDisplay { display_id: DisplayId },
    /// Move was asked to the display the workspace already occupies.
    AlreadyDisplayedThere {
        name: WorkspaceName,
        display_id: DisplayId,
    },
    /// Window management is paused.
    Paused,
    /// The switch would have to move a window, and experimental switching
    /// is not authorised for this topology. Carries the switching status
    /// code and the reason behind it, so the caller is told which of the
    /// four states stands rather than only that it is not `experimental`.
    SwitchingNotAuthorised {
        status: String,
        reason: Option<String>,
    },
    /// Committed state is not durable, so no recovery data could be
    /// promised for the windows the switch would park.
    PersistenceDegraded,
    /// Compensation for an earlier switch left windows unaccounted for.
    /// Switching stays blocked until `restore-switch` reconciles them.
    SwitchDegraded { stranded_windows: usize },
    /// A switch is already in flight; a second one would interleave two
    /// sets of native moves over the same windows.
    SwitchInFlight { display_id: DisplayId },
    /// A window the switch would have to move is full-screen, and no
    /// window is ever forced out of full-screen.
    FullscreenMember {
        name: WorkspaceName,
        window_id: WindowId,
    },
    /// A window the switch would have to move cannot be parked, for a
    /// reason the parking authorisation gives. Reached in preflight, so
    /// nothing has moved.
    MemberNotParkable {
        name: WorkspaceName,
        window_id: WindowId,
        reason: ParkingRefusal,
    },
    /// The display should end up showing no workspace at all, and no
    /// command does that: every switch names a workspace to show. Reached
    /// when undo would have to restore an unfilled display.
    CannotHideWithoutReplacement { display_fingerprint: String },
}

impl WorkspaceRefusal {
    /// A stable machine-readable reason code, shared by IPC and the CLI.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidName { .. } => "invalid_name",
            Self::UnknownWorkspace { .. } => "unknown_workspace",
            Self::AlreadyExists { .. } => "already_exists",
            Self::Displayed { .. } => "displayed",
            Self::NotEmpty { .. } => "not_empty",
            Self::DeclaredByConfiguration { .. } => "declared_by_configuration",
            Self::NotDisplayed { .. } => "not_displayed",
            Self::NoFocusedDisplay => "no_focused_display",
            Self::UnknownDisplay { .. } => "unknown_display",
            Self::AlreadyDisplayedThere { .. } => "already_displayed_there",
            Self::Paused => "paused",
            Self::SwitchingNotAuthorised { .. } => "switching_not_authorised",
            Self::PersistenceDegraded => "persistence_degraded",
            Self::SwitchDegraded { .. } => "switch_degraded",
            Self::SwitchInFlight { .. } => "switch_in_flight",
            Self::FullscreenMember { .. } => "fullscreen_member",
            Self::MemberNotParkable { .. } => "member_not_parkable",
            Self::CannotHideWithoutReplacement { .. } => "cannot_hide_without_replacement",
        }
    }
}

impl std::fmt::Display for WorkspaceRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName { reason } => write!(formatter, "{reason}"),
            Self::UnknownWorkspace { name } => write!(
                formatter,
                "no workspace named {name:?}; create it with `workspace create` or declare it in configuration"
            ),
            Self::AlreadyExists { name } => write!(formatter, "workspace {name} already exists"),
            Self::Displayed { name, display_id } => write!(
                formatter,
                "workspace {name} is displayed on display {}; hide it first",
                display_id.0
            ),
            Self::NotEmpty {
                name,
                live_members,
                dormant_positions,
            } => write!(
                formatter,
                "workspace {name} still owns {live_members} window(s) and {dormant_positions} dormant position(s)"
            ),
            Self::DeclaredByConfiguration { name } => write!(
                formatter,
                "workspace {name} is declared in configuration; remove the declaration to delete it"
            ),
            Self::NotDisplayed { name } => {
                write!(formatter, "workspace {name} is hidden; focus it to display it")
            }
            Self::NoFocusedDisplay => {
                formatter.write_str("no display is focused, so there is nowhere to display the workspace")
            }
            Self::UnknownDisplay { display_id } => {
                write!(formatter, "display {} is not connected", display_id.0)
            }
            Self::AlreadyDisplayedThere { name, display_id } => write!(
                formatter,
                "workspace {name} is already displayed on display {}",
                display_id.0
            ),
            Self::Paused => formatter.write_str("window management is paused"),
            Self::SwitchingNotAuthorised { status, reason } => match reason {
                Some(reason) => write!(
                    formatter,
                    "experimental workspace switching is {status}: {reason}"
                ),
                None => write!(formatter, "experimental workspace switching is {status}"),
            },
            Self::PersistenceDegraded => formatter.write_str(
                "the state database is not durable, so no window may be parked for a switch",
            ),
            Self::SwitchDegraded { stranded_windows } => write!(
                formatter,
                "an earlier switch left {stranded_windows} window(s) unaccounted for; \
                 run `workspace restore-switch` before switching again"
            ),
            Self::SwitchInFlight { display_id } => write!(
                formatter,
                "a workspace switch is already in flight on display {}",
                display_id.0
            ),
            Self::FullscreenMember { name, window_id } => write!(
                formatter,
                "window {} of workspace {name} is full-screen and is never forced out of it",
                window_id.0
            ),
            Self::MemberNotParkable {
                name,
                window_id,
                reason,
            } => write!(
                formatter,
                "window {} of workspace {name} cannot be parked: {reason}",
                window_id.0
            ),
            Self::CannotHideWithoutReplacement {
                display_fingerprint,
            } => write!(
                formatter,
                "display {display_fingerprint:?} showed no workspace, and no command hides one \
                 without showing another"
            ),
        }
    }
}

/// What a successful focus did: displayed a hidden workspace, or focused
/// one that was already on screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceFocusApplied {
    /// The workspace was hidden and is now displayed on `display_id`,
    /// replacing `replaced`, which is now hidden.
    Displayed {
        name: WorkspaceName,
        display_id: DisplayId,
        replaced: Option<WorkspaceName>,
    },
    /// The workspace was already displayed on `display_id`; focus went to
    /// its last-focused live window when it had one, and otherwise the
    /// display itself became the focused display.
    FocusedExisting {
        name: WorkspaceName,
        display_id: DisplayId,
        focused_window: Option<WindowId>,
    },
    /// The switch has windows to move, so it runs as a transaction and has
    /// only started. The displayed assignment does not change until every
    /// move lands; a failure compensates and leaves `replaced` displayed.
    SwitchStarted {
        name: WorkspaceName,
        display_id: DisplayId,
        replaced: Option<WorkspaceName>,
        /// How many windows leave the screen.
        parking: usize,
        /// How many of the target workspace's parked windows come back.
        restoring: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMoveApplied {
    pub name: WorkspaceName,
    pub from_display_id: DisplayId,
    pub to_display_id: DisplayId,
    /// The workspace that was on the target display and now occupies the
    /// source display, so a move never hides anything.
    pub swapped_with: Option<WorkspaceName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreateApplied {
    pub name: WorkspaceName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDeleteApplied {
    pub name: WorkspaceName,
}

/// A workspace switch that could not be completed. The displayed
/// assignment is unchanged either way: what differs is whether every
/// window this transaction had already moved got back where it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSwitchFailed {
    pub name: WorkspaceName,
    pub display_id: DisplayId,
    /// The platform's reason for the move that failed.
    pub reason: String,
    /// Whether compensation put every moved window back.
    pub compensated: bool,
    /// The windows compensation could not account for. Empty when
    /// `compensated`.
    pub stranded_windows: Vec<WindowId>,
}

/// The typed answer to any workspace lifecycle command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceCommandResult {
    Created(WorkspaceCreateApplied),
    Deleted(WorkspaceDeleteApplied),
    Focused(WorkspaceFocusApplied),
    Moved(WorkspaceMoveApplied),
    /// A switch transaction ran and was cancelled. Distinct from
    /// `Refused`, which means nothing moved at all.
    SwitchFailed(WorkspaceSwitchFailed),
    Refused(WorkspaceRefusal),
}

impl WorkspaceCommandResult {
    pub const fn is_applied(&self) -> bool {
        !matches!(self, Self::Refused(_) | Self::SwitchFailed(_))
    }
}

/// Whether the platform adapter has verified a recoverable parking site
/// for the current topology. Parking is never authorised on `Unverified`,
/// and a `Refused` site is never worked around by another hiding
/// mechanism.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ParkingCapability {
    #[default]
    Unverified,
    Verified,
    Refused {
        reason: String,
    },
}

/// Why a requested experimental switching mapping is not in effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceSwitchingUnavailable {
    /// The profile maps a display that is not connected.
    DisplayNotConnected { display_fingerprint: String },
    /// A connected display has no mapping.
    MappingIncomplete { display_fingerprint: String },
    /// The mapping names a workspace the pool does not hold.
    UnknownWorkspace { name: String },
    /// The adapter found no recoverable parking site.
    ParkingRefused { reason: String },
}

impl WorkspaceSwitchingUnavailable {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::DisplayNotConnected { .. } => "display_not_connected",
            Self::MappingIncomplete { .. } => "mapping_incomplete",
            Self::UnknownWorkspace { .. } => "unknown_workspace",
            Self::ParkingRefused { .. } => "parking_refused",
        }
    }
}

impl std::fmt::Display for WorkspaceSwitchingUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DisplayNotConnected {
                display_fingerprint,
            } => write!(
                formatter,
                "the profile maps display {display_fingerprint:?}, which is not connected"
            ),
            Self::MappingIncomplete {
                display_fingerprint,
            } => write!(
                formatter,
                "display {display_fingerprint:?} has no workspace mapped to it"
            ),
            Self::UnknownWorkspace { name } => {
                write!(
                    formatter,
                    "the profile maps workspace {name:?}, which does not exist"
                )
            }
            Self::ParkingRefused { reason } => {
                write!(formatter, "no recoverable parking site: {reason}")
            }
        }
    }
}

/// What still stands between a requested mapping and experimental
/// switching being active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwitchingPending {
    /// No adapter has verified a recoverable parking site yet.
    ParkingCapabilityUnverified,
    /// Recovery data would not be durable, so no window may be parked.
    PersistenceDegraded,
}

impl SwitchingPending {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ParkingCapabilityUnverified => "parking_capability_unverified",
            Self::PersistenceDegraded => "persistence_degraded",
        }
    }
}

/// The state of experimental workspace switching for the current topology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceSwitchingStatus {
    /// No matched profile asks for it. Base config cannot.
    Disabled,
    /// The matched profile asks for it and its mapping is in effect, but
    /// switching itself cannot activate until `pending` clears.
    Requested { pending: SwitchingPending },
    /// The matched profile asks for it, but its mapping could not be
    /// applied; the previous displayed assignment stands.
    Unavailable {
        reason: WorkspaceSwitchingUnavailable,
    },
    /// The mapping is in effect and parking is authorised.
    Experimental,
}

impl WorkspaceSwitchingStatus {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Requested { .. } => "requested",
            Self::Unavailable { .. } => "unavailable",
            Self::Experimental => "experimental",
        }
    }
}

/// Which step of a workspace switch transaction is in flight.
///
/// The two forward phases run in order: everything the outgoing workspace
/// still shows leaves the screen before anything the target workspace
/// parked comes back, so the two sets never overlap. The two compensating
/// phases undo that in the mirror order for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceSwitchPhase {
    /// Recovery data is being recorded for the outgoing windows. No
    /// window has moved yet, so cancelling here moves nothing back.
    Recording,
    /// The outgoing workspace's windows are being parked.
    Parking,
    /// The target workspace's parked windows are being restored.
    Restoring,
    /// A failure was seen; windows this transaction restored are going
    /// back to the parking site.
    CompensatingPark,
    /// Windows this transaction parked are going back on screen.
    CompensatingRestore,
}

impl WorkspaceSwitchPhase {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Parking => "parking",
            Self::Restoring => "restoring",
            Self::CompensatingPark => "compensating_park",
            Self::CompensatingRestore => "compensating_restore",
        }
    }

    /// Whether the transaction is undoing its own work rather than
    /// carrying the switch forward.
    pub const fn is_compensating(&self) -> bool {
        matches!(self, Self::CompensatingPark | Self::CompensatingRestore)
    }
}

/// The health condition a failed compensation leaves behind.
///
/// It names the windows compensation could not account for, because those
/// are what the explicit restore path has to reconcile. Switching stays
/// blocked while this stands: a second switch over windows whose real
/// position is unknown would compound the problem rather than fix it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSwitchDegraded {
    pub display_id: DisplayId,
    /// The workspace the failed switch was trying to display.
    pub target: WorkspaceName,
    /// The workspace that was displayed, and still is: a failed switch
    /// never changes the displayed assignment.
    pub outgoing: Option<WorkspaceName>,
    /// The windows compensation could not put back where they were.
    pub stranded_windows: Vec<WindowId>,
    /// The platform's reason for the failure that started this.
    pub reason: String,
}

/// What an explicit `restore-switch` did about a degraded switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceSwitchRestoreResult {
    /// Every stranded window was put back and switching is unblocked.
    Reconciled { restored_windows: Vec<WindowId> },
    /// Restoration was asked for and windows are on their way back; the
    /// condition clears when the last one lands.
    Requested { windows: Vec<WindowId> },
    /// There was no degraded switch to reconcile.
    NotDegraded,
}

impl WorkspaceSwitchRestoreResult {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Reconciled { .. } => "reconciled",
            Self::Requested { .. } => "requested",
            Self::NotDegraded => "not_degraded",
        }
    }
}

/// The global pool.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WorkspacePool {
    workspaces: BTreeMap<WorkspaceName, Workspace>,
    displayed: HashMap<DisplayId, WorkspaceName>,
    membership: HashMap<WindowId, WorkspaceName>,
}

impl WorkspacePool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty()
    }

    pub fn len(&self) -> usize {
        self.workspaces.len()
    }

    /// Every workspace, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&WorkspaceName, &Workspace)> {
        self.workspaces.iter()
    }

    pub fn names(&self) -> Vec<WorkspaceName> {
        self.workspaces.keys().cloned().collect()
    }

    /// The stored name matching `name` case-insensitively, if any.
    pub fn resolve(&self, name: &str) -> Option<WorkspaceName> {
        let wanted = name.trim().to_lowercase();
        self.workspaces
            .keys()
            .find(|candidate| candidate.key() == wanted)
            .cloned()
    }

    pub fn get(&self, name: &WorkspaceName) -> Option<&Workspace> {
        self.workspaces.get(name)
    }

    pub fn get_mut(&mut self, name: &WorkspaceName) -> Option<&mut Workspace> {
        self.workspaces.get_mut(name)
    }

    pub fn contains(&self, name: &WorkspaceName) -> bool {
        self.workspaces.contains_key(name)
    }

    /// Adds a workspace, hidden and empty. Refuses a name the pool already
    /// holds under any casing.
    pub fn create(
        &mut self,
        name: WorkspaceName,
        origin: WorkspaceOrigin,
    ) -> Result<WorkspaceCreateApplied, WorkspaceRefusal> {
        if let Some(existing) = self.resolve(name.as_str()) {
            return Err(WorkspaceRefusal::AlreadyExists { name: existing });
        }
        self.workspaces.insert(
            name.clone(),
            Workspace {
                origin,
                stashed_tree: None,
                last_focused: None,
            },
        );
        Ok(WorkspaceCreateApplied { name })
    }

    /// Whether deletion would succeed, without deleting. Only a hidden
    /// workspace that owns neither a live member nor a dormant position
    /// may go, and never one configuration declares.
    pub fn check_delete(&self, name: &str) -> Result<WorkspaceName, WorkspaceRefusal> {
        let name = self.require(name)?;
        let workspace = &self.workspaces[&name];
        if let Some(display_id) = self.display_of(&name) {
            return Err(WorkspaceRefusal::Displayed { name, display_id });
        }
        if workspace.origin == WorkspaceOrigin::Configuration {
            return Err(WorkspaceRefusal::DeclaredByConfiguration { name });
        }
        let live_members = self.members_of(&name).len();
        let dormant_positions = workspace
            .stashed_tree
            .as_ref()
            .map_or(0, |tree| tree.dormant_positions().len());
        if live_members > 0 || dormant_positions > 0 {
            return Err(WorkspaceRefusal::NotEmpty {
                name,
                live_members,
                dormant_positions,
            });
        }
        Ok(name)
    }

    pub fn delete(&mut self, name: &str) -> Result<WorkspaceDeleteApplied, WorkspaceRefusal> {
        let name = self.check_delete(name)?;
        self.workspaces.remove(&name);
        Ok(WorkspaceDeleteApplied { name })
    }

    /// The stored name for `name`, or the typed unknown-workspace refusal.
    pub fn require(&self, name: &str) -> Result<WorkspaceName, WorkspaceRefusal> {
        self.resolve(name)
            .ok_or_else(|| WorkspaceRefusal::UnknownWorkspace {
                name: name.trim().to_owned(),
            })
    }

    /// The display `name` is displayed on, if it is displayed at all.
    pub fn display_of(&self, name: &WorkspaceName) -> Option<DisplayId> {
        self.displayed
            .iter()
            .find(|(_, displayed)| *displayed == name)
            .map(|(display_id, _)| *display_id)
    }

    /// The workspace displayed on `display_id`, if any.
    pub fn displayed_on(&self, display_id: DisplayId) -> Option<&WorkspaceName> {
        self.displayed.get(&display_id)
    }

    /// Every displayed assignment, in display order.
    pub fn displayed(&self) -> Vec<(DisplayId, WorkspaceName)> {
        let mut assignments: Vec<_> = self
            .displayed
            .iter()
            .map(|(display_id, name)| (*display_id, name.clone()))
            .collect();
        assignments.sort_by_key(|(display_id, _)| display_id.0);
        assignments
    }

    pub fn is_displayed(&self, name: &WorkspaceName) -> bool {
        self.display_of(name).is_some()
    }

    /// Shows `name` on `display_id`, hiding whatever was there. Returns
    /// the workspace that was displaced, if any. The trees are the
    /// reducer's to exchange: it stashes the displaced tree into the
    /// displaced workspace and takes the shown one's stash out.
    pub fn display(
        &mut self,
        name: &WorkspaceName,
        display_id: DisplayId,
    ) -> Option<WorkspaceName> {
        debug_assert!(self.workspaces.contains_key(name));
        // A workspace is displayed on at most one monitor, so showing it
        // here removes it from wherever else it was.
        self.displayed.retain(|_, displayed| displayed != name);
        self.displayed.insert(display_id, name.clone())
    }

    /// Hides whatever `display_id` shows. Returns the hidden workspace.
    pub fn hide_display(&mut self, display_id: DisplayId) -> Option<WorkspaceName> {
        self.displayed.remove(&display_id)
    }

    /// Exchanges the workspaces on two displays. The caller checks that
    /// `from` is occupied; `to` may be empty, in which case the move
    /// simply leaves `from` empty.
    pub fn swap_displays(&mut self, from: DisplayId, to: DisplayId) {
        let moving = self.displayed.remove(&from);
        let displaced = self.displayed.remove(&to);
        if let Some(moving) = moving {
            self.displayed.insert(to, moving);
        }
        if let Some(displaced) = displaced {
            self.displayed.insert(from, displaced);
        }
    }

    /// The workspace `window_id` belongs to.
    pub fn workspace_of(&self, window_id: WindowId) -> Option<&WorkspaceName> {
        self.membership.get(&window_id)
    }

    /// Every live member of `name`, in window-id order so the answer does
    /// not depend on hash iteration.
    pub fn members_of(&self, name: &WorkspaceName) -> Vec<WindowId> {
        let mut members: Vec<WindowId> = self
            .membership
            .iter()
            .filter(|(_, member_of)| *member_of == name)
            .map(|(window_id, _)| *window_id)
            .collect();
        members.sort_by_key(|window_id| window_id.0);
        members
    }

    /// Makes `window_id` a member of `name`, leaving any previous
    /// workspace. Returns the previous membership.
    pub fn assign(&mut self, window_id: WindowId, name: &WorkspaceName) -> Option<WorkspaceName> {
        debug_assert!(self.workspaces.contains_key(name));
        self.membership.insert(window_id, name.clone())
    }

    /// Forgets `window_id` entirely, as when it closes or leaves
    /// management.
    pub fn unassign(&mut self, window_id: WindowId) -> Option<WorkspaceName> {
        let previous = self.membership.remove(&window_id);
        for workspace in self.workspaces.values_mut() {
            if workspace.last_focused == Some(window_id) {
                workspace.last_focused = None;
            }
        }
        previous
    }

    /// Drops membership for every window not in `live`, and forgets a
    /// last-focused window that is gone.
    pub fn retain_members(&mut self, live: impl Fn(WindowId) -> bool) {
        self.membership.retain(|window_id, _| live(*window_id));
        for workspace in self.workspaces.values_mut() {
            if workspace
                .last_focused
                .is_some_and(|window_id| !live(window_id))
            {
                workspace.last_focused = None;
            }
        }
    }

    /// Records that `window_id` has focus, for the workspace it belongs
    /// to. A window that belongs to no workspace records nothing.
    pub fn note_focus(&mut self, window_id: WindowId) {
        let Some(name) = self.membership.get(&window_id).cloned() else {
            return;
        };
        if let Some(workspace) = self.workspaces.get_mut(&name) {
            workspace.last_focused = Some(window_id);
        }
    }

    /// Drops every displayed assignment whose display is gone, returning
    /// the workspaces that became hidden.
    pub fn retain_displays(&mut self, live: impl Fn(DisplayId) -> bool) -> Vec<WorkspaceName> {
        let mut hidden = Vec::new();
        self.displayed.retain(|display_id, name| {
            let keep = live(*display_id);
            if !keep {
                hidden.push(name.clone());
            }
            keep
        });
        hidden.sort();
        hidden
    }

    /// The hidden workspaces that own no live member, in name order: the
    /// ones an empty display may show without revealing anything.
    pub fn hidden_and_empty(&self) -> Vec<WorkspaceName> {
        self.workspaces
            .keys()
            .filter(|name| !self.is_displayed(name))
            .filter(|name| !self.membership.values().any(|member_of| member_of == *name))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(raw: &str) -> WorkspaceName {
        WorkspaceName::new(raw).unwrap()
    }

    #[test]
    fn names_are_trimmed_and_refuse_empty_long_and_forbidden() {
        assert_eq!(name("  dev ").as_str(), "dev");
        assert_eq!(WorkspaceName::new(" "), Err(WorkspaceNameError::Empty));
        assert_eq!(
            WorkspaceName::new(&"x".repeat(65)),
            Err(WorkspaceNameError::TooLong { chars: 65 })
        );
        assert_eq!(
            WorkspaceName::new("a/b"),
            Err(WorkspaceNameError::ForbiddenCharacter { character: '/' })
        );
        assert_eq!(
            WorkspaceName::new("tab\there"),
            Err(WorkspaceNameError::ForbiddenCharacter { character: '\t' })
        );
    }

    #[test]
    fn names_that_differ_only_by_case_are_one_workspace() {
        let mut pool = WorkspacePool::new();
        pool.create(name("Dev"), WorkspaceOrigin::Command).unwrap();

        assert_eq!(
            pool.create(name("dev"), WorkspaceOrigin::Command),
            Err(WorkspaceRefusal::AlreadyExists { name: name("Dev") })
        );
        assert_eq!(pool.resolve("DEV"), Some(name("Dev")));
    }

    #[test]
    fn naming_an_unknown_workspace_is_a_typed_refusal_not_a_creation() {
        let pool = WorkspacePool::new();

        assert_eq!(
            pool.require("typo"),
            Err(WorkspaceRefusal::UnknownWorkspace {
                name: "typo".to_owned()
            })
        );
        assert!(pool.is_empty(), "a refused name must not create anything");
    }

    #[test]
    fn a_workspace_is_displayed_on_at_most_one_display() {
        let mut pool = WorkspacePool::new();
        pool.create(name("dev"), WorkspaceOrigin::Command).unwrap();
        pool.create(name("chat"), WorkspaceOrigin::Command).unwrap();

        pool.display(&name("dev"), DisplayId(1));
        let displaced = pool.display(&name("dev"), DisplayId(2));

        assert_eq!(displaced, None);
        assert_eq!(pool.display_of(&name("dev")), Some(DisplayId(2)));
        assert_eq!(pool.displayed_on(DisplayId(1)), None);
    }

    #[test]
    fn displaying_over_another_workspace_reports_the_one_it_displaced() {
        let mut pool = WorkspacePool::new();
        pool.create(name("dev"), WorkspaceOrigin::Command).unwrap();
        pool.create(name("chat"), WorkspaceOrigin::Command).unwrap();
        pool.display(&name("dev"), DisplayId(1));

        assert_eq!(pool.display(&name("chat"), DisplayId(1)), Some(name("dev")));
        assert!(!pool.is_displayed(&name("dev")));
    }

    #[test]
    fn delete_refuses_displayed_declared_and_non_empty_workspaces() {
        let mut pool = WorkspacePool::new();
        pool.create(name("dev"), WorkspaceOrigin::Command).unwrap();
        pool.create(name("main"), WorkspaceOrigin::Configuration)
            .unwrap();
        pool.create(name("scratch"), WorkspaceOrigin::Command)
            .unwrap();
        pool.display(&name("dev"), DisplayId(1));
        pool.assign(WindowId(7), &name("scratch"));

        assert_eq!(
            pool.check_delete("dev"),
            Err(WorkspaceRefusal::Displayed {
                name: name("dev"),
                display_id: DisplayId(1)
            })
        );
        assert_eq!(
            pool.check_delete("main"),
            Err(WorkspaceRefusal::DeclaredByConfiguration { name: name("main") })
        );
        assert_eq!(
            pool.check_delete("scratch"),
            Err(WorkspaceRefusal::NotEmpty {
                name: name("scratch"),
                live_members: 1,
                dormant_positions: 0
            })
        );
        assert_eq!(pool.len(), 3, "a refused delete removes nothing");
    }

    #[test]
    fn delete_refuses_a_hidden_workspace_that_still_holds_a_dormant_position() {
        use crate::identity::WindowEvidence;
        use crate::tree::DormantPosition;
        use crate::{ApplicationId, Rect, WindowRole};

        let mut pool = WorkspacePool::new();
        pool.create(name("dev"), WorkspaceOrigin::Command).unwrap();
        let mut tree = ContainerTree::new();
        tree.insert_first(WindowId(1));
        tree.make_dormant(
            &WindowId(1),
            DormantPosition {
                evidence: WindowEvidence {
                    application_id: ApplicationId("code.exe".to_owned()),
                    executable_path: None,
                    native_class: None,
                    role: WindowRole::Normal,
                    launch_order: 0,
                    last_placement: Rect::new(0, 0, 10, 10),
                    display_fingerprint: "D".to_owned(),
                },
                since_unix: 0,
            },
        );
        pool.get_mut(&name("dev")).unwrap().stashed_tree = Some(tree);

        assert_eq!(
            pool.check_delete("dev"),
            Err(WorkspaceRefusal::NotEmpty {
                name: name("dev"),
                live_members: 0,
                dormant_positions: 1
            })
        );
    }

    #[test]
    fn delete_removes_an_empty_hidden_command_workspace() {
        let mut pool = WorkspacePool::new();
        pool.create(name("dev"), WorkspaceOrigin::Command).unwrap();

        assert_eq!(
            pool.delete("dev"),
            Ok(WorkspaceDeleteApplied { name: name("dev") })
        );
        assert!(pool.is_empty());
    }

    #[test]
    fn membership_is_exclusive_and_follows_reassignment() {
        let mut pool = WorkspacePool::new();
        pool.create(name("dev"), WorkspaceOrigin::Command).unwrap();
        pool.create(name("chat"), WorkspaceOrigin::Command).unwrap();

        assert_eq!(pool.assign(WindowId(1), &name("dev")), None);
        assert_eq!(pool.assign(WindowId(1), &name("chat")), Some(name("dev")));
        assert_eq!(pool.members_of(&name("dev")), Vec::<WindowId>::new());
        assert_eq!(pool.members_of(&name("chat")), vec![WindowId(1)]);
    }

    #[test]
    fn last_focus_is_recorded_per_workspace_and_forgotten_with_the_window() {
        let mut pool = WorkspacePool::new();
        pool.create(name("dev"), WorkspaceOrigin::Command).unwrap();
        pool.assign(WindowId(1), &name("dev"));
        pool.assign(WindowId(2), &name("dev"));

        pool.note_focus(WindowId(2));
        assert_eq!(
            pool.get(&name("dev")).unwrap().last_focused,
            Some(WindowId(2))
        );

        pool.note_focus(WindowId(99));
        assert_eq!(
            pool.get(&name("dev")).unwrap().last_focused,
            Some(WindowId(2)),
            "a window that belongs to no workspace records nothing"
        );

        pool.retain_members(|window_id| window_id != WindowId(2));
        assert_eq!(pool.get(&name("dev")).unwrap().last_focused, None);
        assert_eq!(pool.members_of(&name("dev")), vec![WindowId(1)]);
    }

    #[test]
    fn a_vanished_display_hides_its_workspace_without_touching_the_others() {
        let mut pool = WorkspacePool::new();
        pool.create(name("dev"), WorkspaceOrigin::Command).unwrap();
        pool.create(name("chat"), WorkspaceOrigin::Command).unwrap();
        pool.display(&name("dev"), DisplayId(1));
        pool.display(&name("chat"), DisplayId(2));

        let hidden = pool.retain_displays(|display_id| display_id == DisplayId(1));

        assert_eq!(hidden, vec![name("chat")]);
        assert_eq!(pool.display_of(&name("dev")), Some(DisplayId(1)));
        assert_eq!(pool.display_of(&name("chat")), None);
    }

    #[test]
    fn swapping_displays_moves_both_workspaces_and_tolerates_an_empty_target() {
        let mut pool = WorkspacePool::new();
        pool.create(name("dev"), WorkspaceOrigin::Command).unwrap();
        pool.create(name("chat"), WorkspaceOrigin::Command).unwrap();
        pool.display(&name("dev"), DisplayId(1));
        pool.display(&name("chat"), DisplayId(2));

        pool.swap_displays(DisplayId(1), DisplayId(2));
        assert_eq!(pool.display_of(&name("dev")), Some(DisplayId(2)));
        assert_eq!(pool.display_of(&name("chat")), Some(DisplayId(1)));

        pool.swap_displays(DisplayId(2), DisplayId(3));
        assert_eq!(pool.display_of(&name("dev")), Some(DisplayId(3)));
        assert_eq!(pool.displayed_on(DisplayId(2)), None);
    }

    #[test]
    fn hidden_and_empty_lists_only_workspaces_that_would_reveal_nothing() {
        let mut pool = WorkspacePool::new();
        pool.create(name("b-hidden-empty"), WorkspaceOrigin::Command)
            .unwrap();
        pool.create(name("a-hidden-full"), WorkspaceOrigin::Command)
            .unwrap();
        pool.create(name("c-shown"), WorkspaceOrigin::Command)
            .unwrap();
        pool.assign(WindowId(1), &name("a-hidden-full"));
        pool.display(&name("c-shown"), DisplayId(1));

        assert_eq!(pool.hidden_and_empty(), vec![name("b-hidden-empty")]);
    }

    #[test]
    fn refusal_codes_are_distinct() {
        let refusals = [
            WorkspaceRefusal::InvalidName {
                reason: WorkspaceNameError::Empty,
            },
            WorkspaceRefusal::UnknownWorkspace {
                name: "x".to_owned(),
            },
            WorkspaceRefusal::AlreadyExists { name: name("x") },
            WorkspaceRefusal::Displayed {
                name: name("x"),
                display_id: DisplayId(1),
            },
            WorkspaceRefusal::NotEmpty {
                name: name("x"),
                live_members: 0,
                dormant_positions: 0,
            },
            WorkspaceRefusal::DeclaredByConfiguration { name: name("x") },
            WorkspaceRefusal::NotDisplayed { name: name("x") },
            WorkspaceRefusal::NoFocusedDisplay,
            WorkspaceRefusal::UnknownDisplay {
                display_id: DisplayId(1),
            },
            WorkspaceRefusal::AlreadyDisplayedThere {
                name: name("x"),
                display_id: DisplayId(1),
            },
            WorkspaceRefusal::Paused,
        ];
        let mut codes: Vec<&str> = refusals.iter().map(WorkspaceRefusal::code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), refusals.len());
    }
}
