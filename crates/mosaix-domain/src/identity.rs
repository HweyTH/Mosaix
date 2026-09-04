//! Durable window identity: the one definition of "this is the same
//! window" that persistent undo and identity-based scene restoration both
//! use (spec: "one window-identity matcher shared by undo and issue #28").
//!
//! Native handles are ephemeral, so a window that outlives a restart has to
//! be recognised from evidence instead. Two rules shape what that evidence
//! may be. It must be **safe to keep**: raw window titles are not captured,
//! because a title is usually a document name. And it must be **honest
//! about doubt**: the matcher answers `Confident`, `Ambiguous`, or
//! `NoMatch`, and only `Confident` may authorise moving a window. There is
//! deliberately no override that turns the other two into a placement.

use serde::{Deserialize, Serialize};

use crate::geometry::Rect;
use crate::id::{ApplicationId, WindowId};
use crate::window::{Window, WindowRole};

/// Total score for a candidate that agrees on every weighted signal.
pub const MAX_SCORE: u32 = 100;

/// The score a candidate must reach before it can be believed at all.
/// Below this, the best candidate is reported as no match rather than as
/// a weak one, because a weak match is the failure mode this whole module
/// exists to avoid.
pub const CONFIDENT_SCORE: u32 = 60;

/// How far the best candidate must lead the runner-up. Two windows of the
/// same application on the same display routinely tie on everything but
/// placement; without a margin, undo would pick whichever the enumeration
/// happened to return first.
pub const CONFIDENT_MARGIN: u32 = 15;

/// What Mosaix keeps about a window so it can be recognised in a later
/// session.
///
/// Every field here is either the application's own identity or geometry
/// Mosaix itself chose. Nothing derived from a window title is stored --
/// see the module note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowEvidence {
    /// Executable name or bundle identifier.
    pub application_id: ApplicationId,
    /// Full executable path, which separates two builds of one application.
    pub executable_path: Option<String>,
    /// Native window class, which separates a document window from its
    /// application's palettes and dialogs.
    pub native_class: Option<String>,
    /// Semantic role inferred from style flags.
    pub role: WindowRole,
    /// This window's ordinal among its application's windows in visual
    /// order at the time evidence was captured, counting from zero.
    pub launch_order: u32,
    /// Where Mosaix last saw the window.
    pub last_placement: Rect,
    /// The stable fingerprint of the display it was last seen on.
    pub display_fingerprint: String,
}

impl WindowEvidence {
    /// Captures evidence for `window`, which sits at `launch_order` among
    /// its application's windows on the display `display_fingerprint`
    /// names.
    pub fn capture(window: &Window, launch_order: u32, display_fingerprint: &str) -> Self {
        Self {
            application_id: window.application_id.clone(),
            executable_path: window
                .executable_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            native_class: window.native_class.clone(),
            role: window.role,
            launch_order,
            last_placement: window.bounds,
            display_fingerprint: display_fingerprint.to_owned(),
        }
    }
}

/// One signal's contribution to a candidate's score, kept so a refusal can
/// explain itself rather than only naming a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceContribution {
    pub signal: EvidenceSignal,
    pub awarded: u32,
    pub available: u32,
}

/// The weighted signals the matcher compares. Ordered strongest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EvidenceSignal {
    /// Same application. Without this nothing else is worth much.
    Application,
    /// Same native window class.
    NativeClass,
    /// Same executable path.
    ExecutablePath,
    /// Same ordinal among that application's windows.
    LaunchOrder,
    /// Same semantic role.
    Role,
    /// Last-known placement, scored by overlap rather than equality.
    Placement,
}

impl EvidenceSignal {
    /// What agreeing on this signal is worth. The weights sum to
    /// [`MAX_SCORE`].
    pub const fn weight(&self) -> u32 {
        match self {
            Self::Application => 40,
            Self::NativeClass => 20,
            Self::ExecutablePath => 15,
            Self::LaunchOrder => 10,
            Self::Role => 5,
            Self::Placement => 10,
        }
    }

    pub const fn name(&self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::NativeClass => "native_class",
            Self::ExecutablePath => "executable_path",
            Self::LaunchOrder => "launch_order",
            Self::Role => "role",
            Self::Placement => "placement",
        }
    }
}

/// One live window weighed against the stored evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoredCandidate {
    pub window_id: WindowId,
    pub score: u32,
    pub contributions: Vec<EvidenceContribution>,
}

/// The matcher's verdict. Only [`MatchOutcome::Confident`] authorises a
/// persisted placement; there is no way to promote the other two.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchOutcome {
    /// Exactly one candidate cleared [`CONFIDENT_SCORE`] and led the
    /// runner-up by at least [`CONFIDENT_MARGIN`].
    Confident(ScoredCandidate),
    /// Two or more candidates are credible and too close to separate.
    /// Carries them best-first so a refusal can show its working.
    Ambiguous { candidates: Vec<ScoredCandidate> },
    /// Nothing reached [`CONFIDENT_SCORE`]. Carries the best near-misses,
    /// which is what makes "your window is gone" distinguishable from
    /// "your window changed beyond recognition".
    NoMatch { considered: Vec<ScoredCandidate> },
}

impl MatchOutcome {
    /// The window this outcome authorises moving, if any.
    pub fn confident_window(&self) -> Option<WindowId> {
        match self {
            Self::Confident(candidate) => Some(candidate.window_id),
            Self::Ambiguous { .. } | Self::NoMatch { .. } => None,
        }
    }

    /// A stable machine-readable name for the outcome class.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Confident(_) => "confident",
            Self::Ambiguous { .. } => "ambiguous",
            Self::NoMatch { .. } => "no_match",
        }
    }
}

/// How many near-misses a refusal carries. Enough to show why a decision
/// was close, few enough that a refusal payload stays readable.
const REPORTED_CANDIDATES: usize = 4;

/// Weighs `evidence` against every window in `candidates`.
///
/// `display_fingerprint_of` names the display a candidate is currently on,
/// so placement is only compared within the display the evidence was
/// captured on -- the same rectangle on a different monitor is a different
/// place.
pub fn match_window<'a, F>(
    evidence: &WindowEvidence,
    candidates: impl IntoIterator<Item = &'a Window>,
    display_fingerprint_of: F,
) -> MatchOutcome
where
    F: Fn(&Window) -> Option<String>,
{
    let mut scored: Vec<ScoredCandidate> = candidates
        .into_iter()
        .map(|window| {
            let contributions = score_contributions(
                evidence,
                window,
                display_fingerprint_of(window).as_deref(),
                launch_order_unknown(),
            );
            ScoredCandidate {
                window_id: window.id,
                score: contributions.iter().map(|entry| entry.awarded).sum(),
                contributions,
            }
        })
        .collect();
    finish(scored.as_mut_slice())
}

/// [`match_window`], with each candidate's ordinal among its application's
/// windows supplied by the caller. The reducer knows visual window order;
/// this module deliberately does not reconstruct it.
pub fn match_window_with_order<'a, F, O>(
    evidence: &WindowEvidence,
    candidates: impl IntoIterator<Item = &'a Window>,
    display_fingerprint_of: F,
    launch_order_of: O,
) -> MatchOutcome
where
    F: Fn(&Window) -> Option<String>,
    O: Fn(&Window) -> Option<u32>,
{
    let mut scored: Vec<ScoredCandidate> = candidates
        .into_iter()
        .map(|window| {
            let contributions = score_contributions(
                evidence,
                window,
                display_fingerprint_of(window).as_deref(),
                launch_order_of(window),
            );
            ScoredCandidate {
                window_id: window.id,
                score: contributions.iter().map(|entry| entry.awarded).sum(),
                contributions,
            }
        })
        .collect();
    finish(scored.as_mut_slice())
}

const fn launch_order_unknown() -> Option<u32> {
    None
}

fn finish(scored: &mut [ScoredCandidate]) -> MatchOutcome {
    // Ties are broken by window id so that an ambiguous list is reported in
    // the same order every time, which keeps refusal payloads diffable.
    scored.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.window_id.0.cmp(&right.window_id.0))
    });

    let Some(best) = scored.first() else {
        return MatchOutcome::NoMatch {
            considered: Vec::new(),
        };
    };
    if best.score < CONFIDENT_SCORE {
        return MatchOutcome::NoMatch {
            considered: scored.iter().take(REPORTED_CANDIDATES).cloned().collect(),
        };
    }
    let runner_up = scored.get(1).map_or(0, |candidate| candidate.score);
    if best.score - runner_up < CONFIDENT_MARGIN {
        return MatchOutcome::Ambiguous {
            candidates: scored
                .iter()
                .take_while(|candidate| candidate.score >= CONFIDENT_SCORE)
                .take(REPORTED_CANDIDATES)
                .cloned()
                .collect(),
        };
    }
    MatchOutcome::Confident(best.clone())
}

fn score_contributions(
    evidence: &WindowEvidence,
    window: &Window,
    display_fingerprint: Option<&str>,
    launch_order: Option<u32>,
) -> Vec<EvidenceContribution> {
    let mut contributions = Vec::with_capacity(6);
    let mut award = |signal: EvidenceSignal, awarded: u32| {
        contributions.push(EvidenceContribution {
            signal,
            awarded,
            available: signal.weight(),
        });
    };

    award(
        EvidenceSignal::Application,
        if window.application_id == evidence.application_id {
            EvidenceSignal::Application.weight()
        } else {
            0
        },
    );
    // A signal absent from both sides scores nothing. Knowing nothing about
    // either window is not evidence that they are the same window -- and
    // crediting it would let every window of one application clear the
    // threshold on application identity alone.
    award(
        EvidenceSignal::NativeClass,
        match (&evidence.native_class, &window.native_class) {
            (Some(stored), Some(live)) if stored == live => EvidenceSignal::NativeClass.weight(),
            _ => 0,
        },
    );
    award(
        EvidenceSignal::ExecutablePath,
        match (
            &evidence.executable_path,
            window
                .executable_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        ) {
            (Some(stored), Some(live)) if *stored == live => {
                EvidenceSignal::ExecutablePath.weight()
            }
            _ => 0,
        },
    );
    award(
        EvidenceSignal::LaunchOrder,
        match launch_order {
            Some(order) if order == evidence.launch_order => EvidenceSignal::LaunchOrder.weight(),
            _ => 0,
        },
    );
    award(
        EvidenceSignal::Role,
        if window.role == evidence.role {
            EvidenceSignal::Role.weight()
        } else {
            0
        },
    );
    award(
        EvidenceSignal::Placement,
        placement_score(evidence, window, display_fingerprint),
    );

    contributions
}

/// Placement agreement, scaled by how much the two rectangles overlap. A
/// window found on a different display scores nothing: the same rectangle
/// on another monitor is a different place, not a near miss.
fn placement_score(
    evidence: &WindowEvidence,
    window: &Window,
    display_fingerprint: Option<&str>,
) -> u32 {
    if display_fingerprint != Some(evidence.display_fingerprint.as_str()) {
        return 0;
    }
    let stored = evidence.last_placement;
    let live = window.bounds;
    let overlap_width = (stored.right().min(live.right()) - stored.x.max(live.x)).max(0) as i64;
    let overlap_height = (stored.bottom().min(live.bottom()) - stored.y.max(live.y)).max(0) as i64;
    let overlap = overlap_width * overlap_height;
    let union = (stored.width as i64) * (stored.height as i64)
        + (live.width as i64) * (live.height as i64)
        - overlap;
    if union <= 0 {
        return 0;
    }
    let weight = EvidenceSignal::Placement.weight() as i64;
    ((overlap * weight) / union) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::{WindowCapabilities, WindowLifecycle};
    use std::path::PathBuf;

    fn window(id: isize, application: &str, class: &str, bounds: Rect) -> Window {
        Window {
            id: WindowId(id),
            process_id: 100 + id as u32,
            application_id: ApplicationId(application.to_owned()),
            executable_path: Some(PathBuf::from(format!("C:/apps/{application}"))),
            title: "a document nobody should be storing".to_owned(),
            native_class: Some(class.to_owned()),
            role: WindowRole::Normal,
            bounds,
            display_id: crate::id::DisplayId(1),
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

    fn evidence_for(window: &Window, launch_order: u32) -> WindowEvidence {
        WindowEvidence::capture(window, launch_order, "DISPLAY1")
    }

    fn on_display_one(_: &Window) -> Option<String> {
        Some("DISPLAY1".to_owned())
    }

    #[test]
    fn the_weights_sum_to_the_maximum_score() {
        let total: u32 = [
            EvidenceSignal::Application,
            EvidenceSignal::NativeClass,
            EvidenceSignal::ExecutablePath,
            EvidenceSignal::LaunchOrder,
            EvidenceSignal::Role,
            EvidenceSignal::Placement,
        ]
        .iter()
        .map(EvidenceSignal::weight)
        .sum();

        assert_eq!(total, MAX_SCORE);
    }

    #[test]
    fn evidence_never_captures_the_window_title() {
        let subject = window(1, "Code.exe", "Chrome_WidgetWin_1", Rect::new(0, 0, 800, 600));

        let evidence = evidence_for(&subject, 0);
        let stored = serde_json::to_string(&evidence).expect("evidence serializes");

        assert!(
            !stored.contains("a document nobody should be storing"),
            "stored evidence must not contain a raw window title: {stored}"
        );
    }

    #[test]
    fn an_unchanged_window_matches_confidently() {
        let subject = window(1, "Code.exe", "Chrome_WidgetWin_1", Rect::new(0, 0, 800, 600));
        let evidence = evidence_for(&subject, 0);

        let outcome = match_window_with_order(&evidence, [&subject], on_display_one, |_| Some(0));

        match outcome {
            MatchOutcome::Confident(candidate) => {
                assert_eq!(candidate.window_id, WindowId(1));
                assert_eq!(candidate.score, MAX_SCORE);
            }
            other => panic!("expected a confident match, got {other:?}"),
        }
    }

    #[test]
    fn two_identical_windows_of_one_application_are_ambiguous_rather_than_guessed() {
        let first = window(1, "Code.exe", "Chrome_WidgetWin_1", Rect::new(0, 0, 800, 600));
        let second = window(2, "Code.exe", "Chrome_WidgetWin_1", Rect::new(0, 0, 800, 600));
        let evidence = evidence_for(&first, 0);

        let outcome =
            match_window_with_order(&evidence, [&first, &second], on_display_one, |_| Some(0));

        assert_eq!(outcome.confident_window(), None);
        match &outcome {
            MatchOutcome::Ambiguous { candidates } => {
                assert_eq!(candidates.len(), 2, "both twins must be reported");
                assert_eq!(candidates[0].window_id, WindowId(1));
                assert_eq!(candidates[1].window_id, WindowId(2));
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn launch_order_separates_two_windows_of_one_application() {
        let first = window(1, "Code.exe", "Chrome_WidgetWin_1", Rect::new(0, 0, 800, 600));
        let second = window(2, "Code.exe", "Chrome_WidgetWin_1", Rect::new(900, 0, 800, 600));
        let evidence = evidence_for(&first, 0);

        let outcome = match_window_with_order(
            &evidence,
            [&first, &second],
            on_display_one,
            |window| Some(if window.id == WindowId(1) { 0 } else { 1 }),
        );

        assert_eq!(outcome.confident_window(), Some(WindowId(1)));
    }

    #[test]
    fn a_different_application_is_no_match_however_alike_it_sits() {
        let subject = window(1, "Code.exe", "Chrome_WidgetWin_1", Rect::new(0, 0, 800, 600));
        let evidence = evidence_for(&subject, 0);
        let impostor = window(
            2,
            "notepad.exe",
            "Chrome_WidgetWin_1",
            Rect::new(0, 0, 800, 600),
        );

        let outcome = match_window_with_order(&evidence, [&impostor], on_display_one, |_| Some(0));

        match outcome {
            MatchOutcome::NoMatch { considered } => {
                assert_eq!(considered.len(), 1, "the near-miss is worth reporting");
                assert!(considered[0].score < CONFIDENT_SCORE);
            }
            other => panic!("expected no match, got {other:?}"),
        }
    }

    #[test]
    fn no_candidates_at_all_is_no_match_with_nothing_to_report() {
        let subject = window(1, "Code.exe", "Chrome_WidgetWin_1", Rect::new(0, 0, 800, 600));
        let evidence = evidence_for(&subject, 0);

        let outcome = match_window_with_order(&evidence, [], on_display_one, |_| Some(0));

        assert_eq!(
            outcome,
            MatchOutcome::NoMatch {
                considered: Vec::new()
            }
        );
    }

    #[test]
    fn the_same_rectangle_on_another_display_earns_no_placement_credit() {
        let subject = window(1, "Code.exe", "Chrome_WidgetWin_1", Rect::new(0, 0, 800, 600));
        let evidence = evidence_for(&subject, 0);

        let elsewhere =
            match_window_with_order(&evidence, [&subject], |_| Some("DISPLAY2".to_owned()), |_| {
                Some(0)
            });

        let MatchOutcome::Confident(candidate) = elsewhere else {
            panic!("the window is still recognisable, just not where it was");
        };
        let placement = candidate
            .contributions
            .iter()
            .find(|entry| entry.signal == EvidenceSignal::Placement)
            .expect("placement is always scored");
        assert_eq!(placement.awarded, 0);
        assert_eq!(candidate.score, MAX_SCORE - EvidenceSignal::Placement.weight());
    }

    #[test]
    fn contributions_explain_every_signal_that_was_weighed() {
        let subject = window(1, "Code.exe", "Chrome_WidgetWin_1", Rect::new(0, 0, 800, 600));
        let evidence = evidence_for(&subject, 0);

        let outcome = match_window_with_order(&evidence, [&subject], on_display_one, |_| Some(0));
        let MatchOutcome::Confident(candidate) = outcome else {
            panic!("expected a confident match");
        };

        let mut signals: Vec<EvidenceSignal> = candidate
            .contributions
            .iter()
            .map(|entry| entry.signal)
            .collect();
        signals.sort_unstable();
        signals.dedup();
        assert_eq!(
            signals.len(),
            6,
            "every signal must appear in the explanation, including the ones that scored nothing"
        );
        assert!(candidate
            .contributions
            .iter()
            .all(|entry| entry.awarded <= entry.available));
    }

    #[test]
    fn a_sibling_window_of_the_same_application_is_not_a_confident_match() {
        // The failure this guards against: two windows of one application
        // that carry no class and no executable path agree on application
        // and role alone. That must not be enough, or undo would move
        // whichever sibling happened to be enumerated.
        let mut subject = window(1, "Code.exe", "", Rect::new(0, 0, 800, 600));
        subject.native_class = None;
        subject.executable_path = None;
        let evidence = evidence_for(&subject, 0);

        let mut sibling = window(2, "Code.exe", "", Rect::new(900, 0, 800, 600));
        sibling.native_class = None;
        sibling.executable_path = None;

        let outcome = match_window_with_order(&evidence, [&sibling], on_display_one, |_| Some(1));

        assert_eq!(
            outcome.confident_window(),
            None,
            "sharing an application and a role is not proof of being the same window"
        );
    }

    #[test]
    fn an_unknown_launch_order_still_recognises_a_sole_candidate() {
        let subject = window(1, "Code.exe", "Chrome_WidgetWin_1", Rect::new(0, 0, 800, 600));
        let evidence = evidence_for(&subject, 0);

        let outcome = match_window(&evidence, [&subject], on_display_one);

        assert_eq!(
            outcome.confident_window(),
            Some(WindowId(1)),
            "losing one weak signal must not cost recognition of an otherwise exact match"
        );
    }
}
