use std::collections::HashSet;

use serde::{Deserialize, Serialize};

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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorSnapshot {
    pub appearance: Appearance,
    pub display: DisplaySummary,
    pub draft: LayoutDraft,
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
        Self::with_agent(agent::connect())
    }
}

impl EditorSession {
    /// A session talking to `agent`. The connection is opened by the
    /// caller and held here, so the agent sees one connection per settings
    /// window rather than one per request (ADR 0021).
    pub fn with_agent(agent: Box<dyn AgentTransport>) -> Self {
        Self {
            agent,
            snapshot: EditorSnapshot {
                appearance: Appearance::Dark,
                display: DisplaySummary {
                    name: "Studio Display".to_owned(),
                    resolution: "2560 × 1440".to_owned(),
                    scale_percent: 100,
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

    /// Asks the agent to apply the saved layout `draft` names, and reports
    /// success only if the agent confirms it. The editor performs no
    /// placement and writes no configuration file of its own -- the agent
    /// is the authority for both.
    ///
    /// Only the *name* crosses the transport today, so this applies
    /// whichever saved layout configuration holds under that name, not the
    /// cells currently drawn on screen. Persisting a drawn layout back to
    /// configuration -- which is what makes the two the same thing -- is
    /// issue #37. The draft is still validated first, so an unusable
    /// drawing is refused here rather than saved by a later ticket's code
    /// path.
    pub fn apply(&mut self, draft: LayoutDraft) -> Result<CommandReceipt, EditorCommandError> {
        validate_draft(&draft)?;
        self.agent
            .apply_saved_layout(&draft.name)
            .map_err(EditorCommandError::from)?;
        self.revision += 1;
        Ok(CommandReceipt {
            revision: self.revision,
            status: CommandStatus::Applied,
        })
    }

    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.snapshot.appearance = appearance;
    }
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

    /// An agent that answers with whatever the test scripted. The session
    /// owns its transport, so what the agent was asked is recorded through
    /// a handle the test keeps rather than read back off the fake.
    #[derive(Debug)]
    struct FakeAgent {
        outcome: Option<Result<(), AgentError>>,
        applied: Arc<Mutex<Vec<String>>>,
    }

    impl FakeAgent {
        fn answering(outcome: Result<(), AgentError>) -> Self {
            Self {
                outcome: Some(outcome),
                applied: Arc::default(),
            }
        }

        /// A fake with no scripted answer: reaching it is a test failure.
        fn never_asked() -> Self {
            Self {
                outcome: None,
                applied: Arc::default(),
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
    }

    fn session_with(outcome: Result<(), AgentError>) -> EditorSession {
        EditorSession::with_agent(Box::new(FakeAgent::answering(outcome)))
    }

    #[test]
    fn editor_session_previews_locally_without_involving_the_agent() {
        // The fake has no scripted answer, so a preview that reached the
        // agent would panic rather than pass.
        let mut session = EditorSession::with_agent(Box::new(FakeAgent::never_asked()));
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
        let mut session = EditorSession::with_agent(Box::new(agent));
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
    fn an_invalid_draft_is_rejected_before_the_agent_is_asked() {
        // The fake has no scripted answer, so reaching it would panic.
        let mut session = EditorSession::with_agent(Box::new(FakeAgent::never_asked()));
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
}
