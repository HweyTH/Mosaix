use std::collections::HashSet;

use serde::{Deserialize, Serialize};

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
                "the Mosaix agent IPC transport is not available yet; the draft was not saved or applied",
            ),
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
}

impl Default for EditorSession {
    fn default() -> Self {
        Self {
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
}

impl EditorSession {
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

    pub fn apply(&mut self, draft: LayoutDraft) -> Result<CommandReceipt, EditorCommandError> {
        validate_draft(&draft)?;
        Err(EditorCommandError::AgentUnavailable)
    }

    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.snapshot.appearance = appearance;
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

    #[test]
    fn editor_session_previews_locally_but_does_not_claim_an_agent_apply() {
        let mut session = EditorSession::default();
        let mut draft = session.load().draft;
        draft.gap = 20;

        assert_eq!(session.preview(draft.clone()).unwrap().revision, 1);
        assert_eq!(
            session.apply(draft),
            Err(EditorCommandError::AgentUnavailable)
        );
        assert_eq!(session.load().draft.gap, 12);
    }

    #[test]
    fn editor_session_rejects_zones_outside_normalized_work_area() {
        let mut session = EditorSession::default();
        let mut draft = session.load().draft;
        draft.zones[0].width = 1.01;

        assert_eq!(
            session.preview(draft),
            Err(EditorCommandError::ZoneOutsideWorkArea { zone_id: 1 })
        );
    }

    #[test]
    fn editor_session_rejects_overlaps_when_the_draft_disallows_them() {
        let mut session = EditorSession::default();
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
