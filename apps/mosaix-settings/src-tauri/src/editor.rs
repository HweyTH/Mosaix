use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use mosaix_config::LayoutEdit;
use mosaix_domain::NormalizedRect;

use crate::agent::{self, AgentError, AgentTransport};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneDraft {
    pub id: u32,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutDraft {
    pub name: String,
    pub gap: u8,
    pub allow_overlap: bool,
    pub zones: Vec<ZoneDraft>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplaySummary {
    pub name: String,
    pub resolution: String,
    pub scale_percent: u16,
    /// The work area's own pixel dimensions, so a preview is drawn at the
    /// proportions the layout will actually take. A layout's cells are
    /// fractions of this rectangle (ADR 0018).
    pub work_area_width: i32,
    pub work_area_height: i32,
}

/// The display the editor draws against when no adapter could name a real
/// one -- off Windows, or when enumeration failed.
///
/// Nominal on purpose: it is better to draw a canvas and say the display
/// is unknown than to render nothing.
pub fn nominal_display() -> DisplaySummary {
    DisplaySummary {
        name: "No display detected".to_owned(),
        resolution: "1920 × 1080".to_owned(),
        scale_percent: 100,
        work_area_width: 1920,
        work_area_height: 1080,
    }
}

/// One saved layout as the interface lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedLayoutView {
    pub name: String,
    pub cells: Vec<ZoneDraft>,
}

/// What a confirmed configuration write did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutWriteReceipt {
    /// The configuration file the agent wrote.
    ///
    /// Reported because with a profile matched it is not necessarily the
    /// file a user would have guessed: the write goes to the layer that
    /// supplies the layout (ADR 0022). ADR 0022 also wants the
    /// destination shown *before* the save, with a control to redirect it
    /// to base config; that needs per-layout provenance the agent does
    /// not publish yet, and is issue #41.
    pub file: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorSnapshot {
    pub appearance: Appearance,
    /// Every display the layout can be previewed against, primary first.
    /// Never empty: a nominal display stands in when none was found.
    pub displays: Vec<DisplaySummary>,
    pub draft: LayoutDraft,
}

/// One hotkey binding as the interface shows it: what it does, what
/// presses it, and which configuration file supplies it.
///
/// Read-only here. Editing arrives with the capture dialog (issue #40),
/// which is what turns `file` from information into the destination of a
/// write (ADR 0022).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyBindingView {
    /// The command's TOML path: `snap-left`, or `apply-layout.writing`.
    pub command: String,
    /// The saved layout a parameterized binding applies.
    pub layout: Option<String>,
    pub combo: String,
    /// `base`, `profile`, or `unknown`.
    pub source: String,
    /// The configuration file currently supplying this binding, absent
    /// when the agent could not name it.
    pub file: Option<String>,
}

/// Every binding in effect, plus the topology they are in effect for.
///
/// The fingerprint travels with the list because a topology change can
/// swap the matched profile and so change both the combinations and the
/// files behind them. A caller re-reading the list uses it to tell "the
/// same answer again" from "a different desk".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyList {
    pub topology_fingerprint: String,
    pub bindings: Vec<HotkeyBindingView>,
    /// Whether the agent currently has every binding unregistered for a
    /// hotkey editor. Shown rather than inferred: while it is true no
    /// Mosaix hotkey works anywhere on the system, and a user who is not
    /// told will read that as Mosaix having stopped working (ADR 0021).
    pub capture_suspended: bool,
    /// The commands whose bindings did not come back from the last
    /// registration pass, named so a shortcut another application took
    /// during capture is visible rather than merely dead.
    pub unregistered_commands: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandStatus {
    Previewing,
    /// The agent confirmed it applied the layout. Never reported for a
    /// request the agent did not answer.
    Applied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandReceipt {
    pub revision: u64,
    pub status: CommandStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorCommandError {
    AgentUnavailable,
    /// The agent answered and refused. Carries the agent's own reason
    /// rather than a generic failure, because the reason is the only
    /// thing the user can act on.
    AgentRejected {
        reason: String,
    },
    /// The agent speaks a different protocol version. Distinct from a
    /// rejection so the user is told to update rather than left debugging
    /// a feature that cannot work.
    AgentVersionMismatch {
        server_version: u32,
    },
    /// The connection itself failed in a way that is neither an absent
    /// agent nor an answer from one.
    AgentTransportFailed {
        detail: String,
    },
    EmptyLayout,
    EmptyLayoutName,
    DuplicateZoneId {
        zone_id: u32,
    },
    ZoneOutsideWorkArea {
        zone_id: u32,
    },
    OverlappingZones {
        first_zone_id: u32,
        second_zone_id: u32,
    },
}

impl std::fmt::Display for EditorCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AgentUnavailable => formatter.write_str(
                "the Mosaix agent is not running; the layout was not applied",
            ),
            Self::AgentRejected { reason } => {
                write!(formatter, "the Mosaix agent rejected the change: {reason}")
            }
            Self::AgentVersionMismatch { server_version } => write!(
                formatter,
                "the Mosaix agent speaks protocol v{server_version}; update Mosaix so the agent and settings match"
            ),
            Self::AgentTransportFailed { detail } => {
                write!(formatter, "could not reach the Mosaix agent: {detail}")
            }
            Self::EmptyLayout => formatter.write_str("a layout must contain at least one zone"),
            Self::EmptyLayoutName => {
                formatter.write_str("a layout needs a name before it can be saved")
            }
            Self::DuplicateZoneId { zone_id } => {
                write!(formatter, "zone {zone_id} appears more than once")
            }
            Self::ZoneOutsideWorkArea { zone_id } => write!(
                formatter,
                "zone {zone_id} extends outside the normalized work area"
            ),
            Self::OverlappingZones {
                first_zone_id,
                second_zone_id,
            } => write!(
                formatter,
                "zones {first_zone_id} and {second_zone_id} overlap"
            ),
        }
    }
}

#[derive(Debug)]
pub struct EditorSession {
    snapshot: EditorSnapshot,
    preview: Option<LayoutDraft>,
    revision: u64,
    /// The connection to the agent, held for this session's lifetime.
    agent: Box<dyn AgentTransport>,
}

impl Default for EditorSession {
    fn default() -> Self {
        Self::with_agent(agent::connect(), crate::displays::enumerate())
    }
}

impl EditorSession {
    /// A session talking to `agent`. The connection is opened by the
    /// caller and held here, so the agent sees one connection per settings
    /// window rather than one per request (ADR 0021).
    pub fn with_agent(agent: Box<dyn AgentTransport>, displays: Vec<DisplaySummary>) -> Self {
        Self {
            agent,
            snapshot: EditorSnapshot {
                appearance: Appearance::Dark,
                displays: if displays.is_empty() {
                    vec![nominal_display()]
                } else {
                    displays
                },
                draft: LayoutDraft {
                    name: "Developer Focus".to_owned(),
                    gap: 12,
                    allow_overlap: true,
                    zones: vec![
                        ZoneDraft {
                            id: 1,
                            name: "Primary".to_owned(),
                            x: 0.0,
                            y: 0.0,
                            width: 0.62,
                            height: 1.0,
                        },
                        ZoneDraft {
                            id: 2,
                            name: "Reference".to_owned(),
                            x: 0.62,
                            y: 0.0,
                            width: 0.38,
                            height: 0.52,
                        },
                        ZoneDraft {
                            id: 3,
                            name: "Console".to_owned(),
                            x: 0.62,
                            y: 0.52,
                            width: 0.38,
                            height: 0.48,
                        },
                    ],
                },
            },
            preview: None,
            revision: 0,
        }
    }

    pub fn load(&self) -> EditorSnapshot {
        self.snapshot.clone()
    }

    pub fn preview(&mut self, draft: LayoutDraft) -> Result<CommandReceipt, EditorCommandError> {
        validate_draft(&draft)?;
        self.revision += 1;
        self.preview = Some(draft);
        Ok(CommandReceipt {
            revision: self.revision,
            status: CommandStatus::Previewing,
        })
    }

    /// Saves the drawn layout and then applies it, reporting success only
    /// if the agent confirms both. The editor performs no placement and
    /// writes no configuration file of its own -- the agent is the
    /// authority for both.
    ///
    /// The save comes first because only the *name* crosses the transport
    /// on an apply: applying without saving would lay out whichever cells
    /// configuration already held under that name, which is not what a
    /// user looking at their own drawing means by "apply". A save the
    /// agent refuses stops here, so nothing is applied and nothing claims
    /// to have been.
    pub fn apply(&mut self, draft: LayoutDraft) -> Result<CommandReceipt, EditorCommandError> {
        let name = draft.name.clone();
        self.save(draft)?;
        self.agent
            .apply_saved_layout(&name)
            .map_err(EditorCommandError::from)?;
        // The save already advanced the revision. One click on Save &
        // apply is one change, however many requests it takes.
        Ok(CommandReceipt {
            revision: self.revision,
            status: CommandStatus::Applied,
        })
    }

    /// Every hotkey binding the agent currently has in effect, each
    /// naming the file that supplies it.
    ///
    /// Read through the agent rather than off disk: the agent is the
    /// authority for resolved configuration, and reading the files here
    /// would show what is written rather than what is running.
    pub fn hotkeys(&mut self) -> Result<HotkeyList, EditorCommandError> {
        let state = self.agent.state().map_err(EditorCommandError::from)?;
        Ok(HotkeyList {
            topology_fingerprint: state.topology_fingerprint,
            capture_suspended: state.hotkey_capture_suspended,
            unregistered_commands: state.unregistered_bindings,
            bindings: state
                .hotkeys
                .into_iter()
                .map(|binding| HotkeyBindingView {
                    command: binding.command,
                    layout: binding.layout,
                    combo: binding.combo,
                    source: binding.source,
                    file: binding.file,
                })
                .collect(),
        })
    }

    /// Opens hotkey capture: the agent unregisters every binding until
    /// this session closes it, or until the connection ends (ADR 0021).
    ///
    /// Held for the editor's lifetime rather than for one dialog, so
    /// alt-tabbing away does not churn `RegisterHotKey` and risk losing a
    /// combination to another application on each cycle.
    pub fn start_hotkey_capture(&mut self) -> Result<(), EditorCommandError> {
        self.agent
            .start_hotkey_capture()
            .map_err(EditorCommandError::from)
    }

    /// Closes hotkey capture, so the agent registers the bindings again
    /// and reports which of them came back.
    pub fn end_hotkey_capture(&mut self) -> Result<(), EditorCommandError> {
        self.agent
            .end_hotkey_capture()
            .map_err(EditorCommandError::from)
    }

    /// The saved layouts the agent currently has, by name.
    pub fn layouts(&mut self) -> Result<Vec<SavedLayoutView>, EditorCommandError> {
        let state = self.agent.state().map_err(EditorCommandError::from)?;
        Ok(state
            .saved_layouts
            .into_iter()
            .map(|(name, layout)| SavedLayoutView {
                name,
                cells: layout
                    .cells
                    .into_iter()
                    .enumerate()
                    .map(|(index, cell)| ZoneDraft {
                        id: index as u32 + 1,
                        name: format!("Zone {}", index + 1),
                        x: cell.x,
                        y: cell.y,
                        width: cell.width,
                        height: cell.height,
                    })
                    .collect(),
            })
            .collect())
    }

    /// Saves `draft` as the saved layout it names, and reports the file
    /// the agent wrote.
    ///
    /// The drawing is checked here so an unusable one is refused before
    /// the agent is asked; the name is checked for the two problems that
    /// have nothing to do with the rest of the configuration, so the
    /// common mistakes are reported before the write rather than as a
    /// rejected candidate afterwards. Everything else -- a name already
    /// taken, a layout declared in two layers -- is the agent's to
    /// answer, because only the agent can see the whole directory.
    pub fn save(&mut self, draft: LayoutDraft) -> Result<LayoutWriteReceipt, EditorCommandError> {
        validate_draft(&draft)?;
        check_name(&draft.name)?;
        self.edit(LayoutEdit::Save {
            name: draft.name.clone(),
            cells: draft
                .zones
                .iter()
                .map(|zone| NormalizedRect {
                    x: zone.x,
                    y: zone.y,
                    width: zone.width,
                    height: zone.height,
                })
                .collect(),
        })
    }

    /// Renames a saved layout. The write lands in whichever file
    /// declares it, which the receipt names.
    pub fn rename(
        &mut self,
        from: &str,
        to: &str,
    ) -> Result<LayoutWriteReceipt, EditorCommandError> {
        check_name(to)?;
        self.edit(LayoutEdit::Rename {
            from: from.to_owned(),
            to: to.to_owned(),
        })
    }

    /// Copies a saved layout under a new name, beside the original --
    /// a variant of a desk-specific layout stays desk-specific.
    pub fn duplicate(
        &mut self,
        from: &str,
        to: &str,
    ) -> Result<LayoutWriteReceipt, EditorCommandError> {
        check_name(to)?;
        self.edit(LayoutEdit::Duplicate {
            from: from.to_owned(),
            to: to.to_owned(),
        })
    }

    /// Removes a saved layout. A name both configuration layers declare
    /// is refused by the agent rather than half-removed.
    pub fn delete(&mut self, name: &str) -> Result<LayoutWriteReceipt, EditorCommandError> {
        self.edit(LayoutEdit::Delete {
            name: name.to_owned(),
        })
    }

    fn edit(&mut self, edit: LayoutEdit) -> Result<LayoutWriteReceipt, EditorCommandError> {
        let file = self
            .agent
            .edit_layouts(edit)
            .map_err(EditorCommandError::from)?;
        self.revision += 1;
        Ok(LayoutWriteReceipt { file })
    }

    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.snapshot.appearance = appearance;
    }
}

/// Rejects a layout name that is empty or whitespace-only.
///
/// The one name rule the settings application can check on its own:
/// whether a name is *taken* depends on the whole configuration
/// directory, which only the agent has.
fn check_name(name: &str) -> Result<(), EditorCommandError> {
    if name.trim().is_empty() {
        return Err(EditorCommandError::EmptyLayoutName);
    }
    Ok(())
}

impl From<AgentError> for EditorCommandError {
    fn from(error: AgentError) -> Self {
        match error {
            AgentError::Unavailable => Self::AgentUnavailable,
            AgentError::Rejected { reason } => Self::AgentRejected { reason },
            AgentError::VersionMismatch { server_version } => {
                Self::AgentVersionMismatch { server_version }
            }
            AgentError::Transport { detail } => Self::AgentTransportFailed { detail },
        }
    }
}

fn validate_draft(draft: &LayoutDraft) -> Result<(), EditorCommandError> {
    if draft.zones.is_empty() {
        return Err(EditorCommandError::EmptyLayout);
    }

    let mut ids = HashSet::new();
    for zone in &draft.zones {
        if !ids.insert(zone.id) {
            return Err(EditorCommandError::DuplicateZoneId { zone_id: zone.id });
        }
        if zone.x < 0.0
            || zone.y < 0.0
            || zone.width <= 0.0
            || zone.height <= 0.0
            || zone.x + zone.width > 1.0
            || zone.y + zone.height > 1.0
        {
            return Err(EditorCommandError::ZoneOutsideWorkArea { zone_id: zone.id });
        }
    }

    if !draft.allow_overlap {
        for (index, first) in draft.zones.iter().enumerate() {
            for second in &draft.zones[index + 1..] {
                let overlaps = first.x < second.x + second.width
                    && first.x + first.width > second.x
                    && first.y < second.y + second.height
                    && first.y + first.height > second.y;
                if overlaps {
                    return Err(EditorCommandError::OverlappingZones {
                        first_zone_id: first.id,
                        second_zone_id: second.id,
                    });
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    use mosaix_ipc::StateSnapshot;

    /// An agent that answers with whatever the test scripted. The session
    /// owns its transport, so what the agent was asked is recorded through
    /// a handle the test keeps rather than read back off the fake.
    #[derive(Debug, Default)]
    struct FakeAgent {
        outcome: Option<Result<(), AgentError>>,
        applied: Arc<Mutex<Vec<String>>>,
        state: Option<Result<StateSnapshot, AgentError>>,
        edits: Arc<Mutex<Vec<LayoutEdit>>>,
        edit_outcome: Option<Result<String, AgentError>>,
        capture: Arc<Mutex<Vec<bool>>>,
    }

    impl FakeAgent {
        /// A fake that confirms saves and answers an apply with `outcome`
        /// -- the shape every apply test wants, since an apply now saves
        /// first.
        fn answering(outcome: Result<(), AgentError>) -> Self {
            Self {
                outcome: Some(outcome),
                edit_outcome: Some(Ok("config.toml".to_owned())),
                ..Self::default()
            }
        }

        /// A fake with no scripted answer: reaching it is a test failure.
        fn never_asked() -> Self {
            Self::default()
        }

        fn reporting(state: Result<StateSnapshot, AgentError>) -> Self {
            Self {
                state: Some(state),
                ..Self::default()
            }
        }

        fn writing(outcome: Result<String, AgentError>) -> Self {
            Self {
                edit_outcome: Some(outcome),
                ..Self::default()
            }
        }
    }

    impl AgentTransport for FakeAgent {
        fn apply_saved_layout(&mut self, name: &str) -> Result<(), AgentError> {
            self.applied.lock().unwrap().push(name.to_owned());
            self.outcome
                .clone()
                .expect("the test scripted no answer for this request")
        }

        fn state(&mut self) -> Result<StateSnapshot, AgentError> {
            self.state
                .clone()
                .expect("the test scripted no state for this request")
        }

        fn edit_layouts(&mut self, edit: LayoutEdit) -> Result<String, AgentError> {
            self.edits.lock().unwrap().push(edit);
            self.edit_outcome
                .clone()
                .expect("the test scripted no answer for this edit")
        }

        fn start_hotkey_capture(&mut self) -> Result<(), AgentError> {
            self.capture.lock().unwrap().push(true);
            Ok(())
        }

        fn end_hotkey_capture(&mut self) -> Result<(), AgentError> {
            self.capture.lock().unwrap().push(false);
            Ok(())
        }
    }

    /// A state snapshot carrying `bindings` and nothing else of interest.
    fn state_reporting(
        fingerprint: &str,
        bindings: Vec<mosaix_ipc::HotkeyBindingSnapshot>,
    ) -> StateSnapshot {
        let mut snapshot = StateSnapshot::from(mosaix_engine::EngineState::default());
        snapshot.topology_fingerprint = fingerprint.to_owned();
        snapshot.hotkeys = bindings;
        snapshot
    }

    fn binding(
        command: &str,
        combo: &str,
        source: &str,
        file: &str,
    ) -> mosaix_ipc::HotkeyBindingSnapshot {
        mosaix_ipc::HotkeyBindingSnapshot {
            command: command.to_owned(),
            layout: None,
            combo: combo.to_owned(),
            source: source.to_owned(),
            file: Some(file.to_owned()),
        }
    }

    #[test]
    fn the_hotkey_list_carries_each_bindings_supplying_file() {
        let mut session = session(FakeAgent::reporting(Ok(state_reporting(
            "MON-A@0,0 1920x1080 scale=1",
            vec![
                binding("snap-left", "ctrl+alt+left", "base", "config.toml"),
                binding("snap-right", "ctrl+shift+right", "profile", "desk.toml"),
            ],
        ))));

        let list = session.hotkeys().expect("the agent answered");

        assert_eq!(list.topology_fingerprint, "MON-A@0,0 1920x1080 scale=1");
        assert_eq!(list.bindings[0].file.as_deref(), Some("config.toml"));
        assert_eq!(list.bindings[0].source, "base");
        assert_eq!(
            list.bindings[1].file.as_deref(),
            Some("desk.toml"),
            "a profile-supplied binding names the profile, which is where an edit would land"
        );
        assert_eq!(list.bindings[1].source, "profile");
    }

    #[test]
    fn saving_a_drawn_layout_sends_its_cells_and_reports_the_file_written() {
        let agent = FakeAgent::writing(Ok("desk.toml".to_owned()));
        let edits = Arc::clone(&agent.edits);
        let mut session = session(agent);
        let mut draft = session.load().draft;
        draft.name = "Writing".to_owned();

        let receipt = session.save(draft).expect("a confirmed save");

        assert_eq!(
            receipt.file, "desk.toml",
            "the destination is worth reporting: with a profile matched it is not the obvious file"
        );
        let sent = edits.lock().unwrap().clone();
        match &sent[0] {
            LayoutEdit::Save { name, cells } => {
                assert_eq!(name, "Writing");
                assert_eq!(cells.len(), 3, "the drawn cells, not just the name");
                assert_eq!(cells[0].width, 0.62);
            }
            other => panic!("expected a save, got {other:?}"),
        }
    }

    #[test]
    fn a_save_the_agent_rejected_is_a_failure_carrying_its_reason() {
        let mut session = session(FakeAgent::writing(Err(AgentError::Rejected {
            reason:
                "config.toml: saved layout \"writing\" cell 0 falls outside a display work area"
                    .to_owned(),
        })));
        let draft = session.load().draft;

        let error = session.save(draft).unwrap_err();

        assert!(
            error.to_string().contains("falls outside"),
            "the agent's own reason is the only thing the user can act on, got {error}"
        );
    }

    #[test]
    fn a_layout_with_no_name_is_refused_before_the_agent_is_asked() {
        // The fake has no scripted answer, so reaching it would panic.
        let mut session = session(FakeAgent::never_asked());
        let mut draft = session.load().draft;
        draft.name = "   ".to_owned();

        assert_eq!(
            session.save(draft),
            Err(EditorCommandError::EmptyLayoutName)
        );
    }

    #[test]
    fn renaming_duplicating_and_deleting_each_reach_the_agent_as_themselves() {
        for (act, expected) in [
            (
                Box::new(|session: &mut EditorSession| session.rename("draft", "writing"))
                    as Box<dyn Fn(&mut EditorSession) -> _>,
                LayoutEdit::Rename {
                    from: "draft".to_owned(),
                    to: "writing".to_owned(),
                },
            ),
            (
                Box::new(|session: &mut EditorSession| {
                    session.duplicate("writing", "writing wide")
                }),
                LayoutEdit::Duplicate {
                    from: "writing".to_owned(),
                    to: "writing wide".to_owned(),
                },
            ),
            (
                Box::new(|session: &mut EditorSession| session.delete("writing")),
                LayoutEdit::Delete {
                    name: "writing".to_owned(),
                },
            ),
        ] {
            let agent = FakeAgent::writing(Ok("config.toml".to_owned()));
            let edits = Arc::clone(&agent.edits);
            let mut session = session(agent);

            act(&mut session).expect("a confirmed edit");

            assert_eq!(edits.lock().unwrap()[0], expected);
        }
    }

    #[test]
    fn renaming_to_an_empty_name_is_refused_before_the_agent_is_asked() {
        let mut session = session(FakeAgent::never_asked());

        assert_eq!(
            session.rename("writing", "  "),
            Err(EditorCommandError::EmptyLayoutName)
        );
    }

    #[test]
    fn the_saved_layout_list_comes_from_the_agent_with_its_cells() {
        let mut state = state_reporting("MON-A", Vec::new());
        state.saved_layouts.insert(
            "writing".to_owned(),
            mosaix_config::SavedLayout {
                cells: vec![
                    mosaix_domain::NormalizedRect {
                        x: 0.0,
                        y: 0.0,
                        width: 0.6,
                        height: 1.0,
                    },
                    mosaix_domain::NormalizedRect {
                        x: 0.6,
                        y: 0.0,
                        width: 0.4,
                        height: 1.0,
                    },
                ],
            },
        );
        let mut session = session(FakeAgent::reporting(Ok(state)));

        let layouts = session.layouts().expect("the agent answered");

        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "writing");
        assert_eq!(layouts[0].cells.len(), 2);
        assert_eq!(layouts[0].cells[1].x, 0.6);
        assert_eq!(
            layouts[0].cells[0].id, 1,
            "cells get ids so the editor can select one after loading it"
        );
    }

    #[test]
    fn the_editor_offers_the_real_displays_a_layout_can_be_previewed_against() {
        let session = session(FakeAgent::never_asked());

        let displays = session.load().displays;

        assert_eq!(displays[0].name, "Primary display");
        assert_eq!(
            (displays[0].work_area_width, displays[0].work_area_height),
            (2560, 1400),
            "a preview drawn at the wrong proportions shows the wrong shape"
        );
        assert_eq!(displays[1].work_area_width, 1920);
    }

    #[test]
    fn an_editor_with_no_displays_found_still_has_one_to_draw_against() {
        let session = EditorSession::with_agent(Box::new(FakeAgent::never_asked()), Vec::new());

        let displays = session.load().displays;

        assert_eq!(displays.len(), 1);
        assert_eq!(displays[0].name, "No display detected");
    }

    #[test]
    fn the_hotkey_list_reports_an_absent_agent_rather_than_an_empty_list() {
        // An empty list and "no agent to ask" look identical on screen
        // unless the second one is an error.
        let mut session = session(FakeAgent::reporting(Err(AgentError::Unavailable)));

        assert_eq!(
            session.hotkeys().unwrap_err(),
            EditorCommandError::AgentUnavailable
        );
    }

    #[test]
    fn the_hotkey_list_keeps_a_version_mismatch_distinct_from_a_rejection() {
        let mut session = session(FakeAgent::reporting(Err(AgentError::VersionMismatch {
            server_version: 1,
        })));

        assert_eq!(
            session.hotkeys().unwrap_err(),
            EditorCommandError::AgentVersionMismatch { server_version: 1 }
        );
    }

    /// Two displays of different shapes, so a test can tell which one a
    /// preview was drawn against.
    fn test_displays() -> Vec<DisplaySummary> {
        vec![
            DisplaySummary {
                name: "Primary display".to_owned(),
                resolution: "2560 × 1400".to_owned(),
                scale_percent: 100,
                work_area_width: 2560,
                work_area_height: 1400,
            },
            DisplaySummary {
                name: "Display 2".to_owned(),
                resolution: "1920 × 1040".to_owned(),
                scale_percent: 125,
                work_area_width: 1920,
                work_area_height: 1040,
            },
        ]
    }

    fn session(agent: FakeAgent) -> EditorSession {
        EditorSession::with_agent(Box::new(agent), test_displays())
    }

    fn session_with(outcome: Result<(), AgentError>) -> EditorSession {
        session(FakeAgent::answering(outcome))
    }

    #[test]
    fn editor_session_previews_locally_without_involving_the_agent() {
        // The fake has no scripted answer, so a preview that reached the
        // agent would panic rather than pass.
        let mut session = session(FakeAgent::never_asked());
        let mut draft = session.load().draft;
        draft.gap = 20;

        let receipt = session.preview(draft).unwrap();

        assert_eq!(receipt.revision, 1);
        assert_eq!(receipt.status, CommandStatus::Previewing);
        assert_eq!(session.load().draft.gap, 12);
    }

    #[test]
    fn a_confirmed_apply_is_reported_as_applied() {
        let mut session = session_with(Ok(()));
        let draft = session.load().draft;

        let receipt = session.apply(draft).expect("a confirmed apply succeeds");

        assert_eq!(receipt.status, CommandStatus::Applied);
        assert_eq!(receipt.revision, 1);
    }

    #[test]
    fn an_apply_asks_the_agent_for_the_drafts_own_layout_name() {
        let agent = FakeAgent::answering(Ok(()));
        let applied = Arc::clone(&agent.applied);
        let mut session = session(agent);
        let mut draft = session.load().draft;
        draft.name = "Writing".to_owned();

        session.apply(draft).unwrap();

        assert_eq!(*applied.lock().unwrap(), vec!["Writing".to_owned()]);
    }

    #[test]
    fn a_rejected_apply_surfaces_the_agents_own_reason() {
        let mut session = session_with(Err(AgentError::Rejected {
            reason: "no managed window is focused, so there is no display to apply a layout to"
                .to_owned(),
        }));
        let draft = session.load().draft;

        let error = session.apply(draft).unwrap_err();

        assert_eq!(
            error,
            EditorCommandError::AgentRejected {
                reason: "no managed window is focused, so there is no display to apply a layout to"
                    .to_owned(),
            }
        );
        assert!(
            error.to_string().contains("no managed window is focused"),
            "the message a user sees must carry the agent's reason, got {error}"
        );
    }

    #[test]
    fn a_version_mismatch_is_reported_apart_from_a_rejection() {
        let mut session = session_with(Err(AgentError::VersionMismatch { server_version: 1 }));
        let draft = session.load().draft;

        let error = session.apply(draft).unwrap_err();

        assert_eq!(
            error,
            EditorCommandError::AgentVersionMismatch { server_version: 1 }
        );
        assert!(
            error.to_string().contains("update"),
            "a version mismatch must tell the user to update, got {error}"
        );
    }

    #[test]
    fn an_absent_agent_is_reported_as_unavailable_rather_than_as_a_success() {
        let mut session = session_with(Err(AgentError::Unavailable));
        let draft = session.load().draft;

        assert_eq!(
            session.apply(draft),
            Err(EditorCommandError::AgentUnavailable)
        );
    }

    #[test]
    fn an_apply_saves_the_drawing_before_applying_it() {
        // Applying sends only a name, so a draft that was never saved
        // would lay out whatever configuration already held under it.
        let agent = FakeAgent::answering(Ok(()));
        let edits = Arc::clone(&agent.edits);
        let applied = Arc::clone(&agent.applied);
        let mut session = session(agent);
        let mut draft = session.load().draft;
        draft.name = "Writing".to_owned();

        session.apply(draft).expect("a confirmed save and apply");

        assert!(
            matches!(&edits.lock().unwrap()[0], LayoutEdit::Save { name, .. } if name == "Writing"),
            "the drawn cells must be persisted first"
        );
        assert_eq!(*applied.lock().unwrap(), vec!["Writing".to_owned()]);
    }

    #[test]
    fn an_apply_whose_save_was_refused_never_reaches_the_apply() {
        let agent = FakeAgent {
            edit_outcome: Some(Err(AgentError::Rejected {
                reason: "config.toml: saved layout \"writing\" declares no cells".to_owned(),
            })),
            // No scripted apply answer: reaching it would panic.
            ..FakeAgent::default()
        };
        let applied = Arc::clone(&agent.applied);
        let mut session = session(agent);
        let draft = session.load().draft;

        let error = session.apply(draft).unwrap_err();

        assert!(
            error.to_string().contains("declares no cells"),
            "got {error}"
        );
        assert!(
            applied.lock().unwrap().is_empty(),
            "nothing should be applied after a refused save"
        );
    }

    #[test]
    fn an_invalid_draft_is_rejected_before_the_agent_is_asked() {
        // The fake has no scripted answer, so reaching it would panic.
        let mut session = session(FakeAgent::never_asked());
        let mut draft = session.load().draft;
        draft.zones.clear();

        assert_eq!(session.apply(draft), Err(EditorCommandError::EmptyLayout));
    }

    #[test]
    fn editor_session_rejects_zones_outside_normalized_work_area() {
        let mut session = session_with(Ok(()));
        let mut draft = session.load().draft;
        draft.zones[0].width = 1.01;

        assert_eq!(
            session.preview(draft),
            Err(EditorCommandError::ZoneOutsideWorkArea { zone_id: 1 })
        );
    }

    #[test]
    fn editor_session_rejects_overlaps_when_the_draft_disallows_them() {
        let mut session = session_with(Ok(()));
        let mut draft = session.load().draft;
        draft.allow_overlap = false;
        draft.zones.push(ZoneDraft {
            id: 4,
            name: "Overlapping".to_owned(),
            x: 0.5,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        });

        assert_eq!(
            session.apply(draft),
            Err(EditorCommandError::OverlappingZones {
                first_zone_id: 1,
                second_zone_id: 4,
            })
        );
    }

    #[test]
    fn opening_and_closing_the_editor_asks_the_agent_to_suspend_and_resume() {
        let agent = FakeAgent::default();
        let capture = Arc::clone(&agent.capture);
        let mut session = session(agent);

        session.start_hotkey_capture().expect("the agent answered");
        session.end_hotkey_capture().expect("the agent answered");

        assert_eq!(
            *capture.lock().unwrap(),
            vec![true, false],
            "the editor asks for suspension when it opens and releases it when it closes"
        );
    }

    #[test]
    fn the_hotkey_list_states_that_capture_has_registration_suspended() {
        let mut state = state_reporting("MON-A", vec![binding("snap-left", "ctrl+alt+left", "base", "config.toml")]);
        state.hotkey_capture_suspended = true;
        state.unregistered_bindings = vec!["snap-right".to_owned()];
        let mut session = session(FakeAgent::reporting(Ok(state)));

        let list = session.hotkeys().expect("the agent answered");

        assert!(
            list.capture_suspended,
            "a user whose hotkeys have stopped working is told why rather than left to infer it"
        );
        assert_eq!(list.unregistered_commands, vec!["snap-right".to_owned()]);
    }
}
