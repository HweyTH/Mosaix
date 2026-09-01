//! Focus border: deciding what to outline (CONTEXT.md "Focus border").
//!
//! The whole policy is one pure function over an [`EngineState`] snapshot,
//! so every rule below is testable without a window server. Painting is
//! `mosaix_platform_windows::FocusBorderOverlay`'s job; driving the two
//! together is [`crate::focus_border_controller`]'s.

use mosaix_config::{FocusBorderSection, Rgb, DEFAULT_BORDER_COLOR};
use mosaix_domain::{Rect, WindowLifecycle};
use mosaix_engine::EngineState;

/// Everything needed to paint the border for one moment of engine state.
/// Bundled so the controller can compare a new target with the current one
/// and skip repainting when nothing a viewer could see has changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusBorderTarget {
    /// The focused window's bounds. The border is drawn *inside* these --
    /// see `FocusBorderOverlay` -- so it never reaches into a gap or over a
    /// neighbouring window, whatever the configured gaps are.
    pub bounds: Rect,
    pub color: Rgb,
    pub thickness: i32,
}

/// What the focus border should show right now, or `None` if it should be
/// hidden.
///
/// The visibility rules, in the order they're applied:
///
/// 1. `[focus_border] enabled = false` hides it outright.
/// 2. It is shown only while automatic tiling is actually running, which
///    `automatic_tiling_active` already means -- that flag is
///    `automatic_tiling_enabled && !automatic_tiling_suspended`. So manual
///    (no tiling-enabled profile matched) and suspended both hide it
///    without this function re-deriving either condition.
/// 3. Pause hides it. Together with (2) this is the same guard the reducer
///    uses before a grid reflow, so the border is visible exactly when
///    tiling is live.
/// 4. The focused window must be *managed*. `Exclude` windows are already
///    absent from `inventory`, so membership is the whole check -- and
///    because `inventory` holds `Float` as well as `Tile`, a floating
///    window still gets a border.
/// 5. It must also be on screen. `inventory` keeps minimized, hidden, and
///    cloaked windows (they are temporarily ineligible, not unmanaged), so
///    membership alone would leave a border painted on bare desktop after a
///    minimize or a virtual-desktop switch that moves focus nowhere.
///    Maximized and full-screen windows are visible and do get one.
/// 6. The focused window must not be the one currently being dragged or
///    resized. The engine defers placement for the duration of an
///    interactive placement session (ADR 0016), so a border drawn now would
///    sit where the window was until the user lets go. Hiding is the honest
///    option, and the snap preview is the overlay doing the talking during
///    a drag anyway. A drag of some *other* window changes nothing.
/// 7. The engine must know where that window is.
pub fn focus_border_target(state: &EngineState) -> Option<FocusBorderTarget> {
    let style = &state.resolved_config.focus_border;
    if !style.enabled || !state.automatic_tiling_active || state.paused {
        return None;
    }

    let window_id = state.focused_window?;
    let managed = state.inventory.get(&window_id)?;
    if !is_on_screen(managed.window.lifecycle) {
        return None;
    }
    if state
        .interactive_placement
        .is_some_and(|session| session.window_id == window_id)
    {
        return None;
    }
    // `observed_bounds`, not `bounds`: the latter is the placement Mosaix
    // last *intended*, which stays deliberately stale so the engine can
    // detect external moves by comparing the two (ADR 0001). A border drawn
    // from it would stay behind any window something else repositioned --
    // exactly the floating windows this feature is meant to cover.
    let bounds = state.windows.get(&window_id)?.observed_bounds;

    Some(FocusBorderTarget {
        bounds,
        color: resolved_color(style),
        thickness: style.thickness,
    })
}

/// Whether a window in this lifecycle state has pixels on screen to outline.
fn is_on_screen(lifecycle: WindowLifecycle) -> bool {
    match lifecycle {
        WindowLifecycle::Active | WindowLifecycle::Maximized | WindowLifecycle::Fullscreen => true,
        WindowLifecycle::Minimized | WindowLifecycle::Hidden | WindowLifecycle::Cloaked => false,
    }
}

/// `mosaix_config::validate` rejects a config whose color doesn't parse, so
/// the fallback here is unreachable through the normal config path. It
/// exists because [`EngineState`]'s `Default` (used before the first config
/// load completes) and a hand-built `ResolvedConfig` can both bypass
/// validation, and a border in the wrong color beats no border at all.
fn resolved_color(style: &FocusBorderSection) -> Rgb {
    Rgb::parse(&style.color).unwrap_or_else(|| {
        Rgb::parse(DEFAULT_BORDER_COLOR).expect("the default border color is valid hex")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaix_domain::{
        ApplicationId, DisplayId, Window, WindowCapabilities, WindowId, WindowLifecycle, WindowRole,
    };
    use mosaix_engine::{
        EligibilityReason, InteractivePlacementSession, ManagedWindow, WindowPlacement,
    };
    use mosaix_rules::ManageAction;

    const BOUNDS: Rect = Rect {
        x: 100,
        y: 200,
        width: 800,
        height: 600,
    };

    fn window(id: i32) -> Window {
        Window {
            id: WindowId(id as isize),
            process_id: 1,
            application_id: ApplicationId("test".to_string()),
            executable_path: None,
            title: "non-sensitive-test-title".to_string(),
            native_class: None,
            role: WindowRole::Normal,
            bounds: BOUNDS,
            display_id: DisplayId(1),
            capabilities: WindowCapabilities {
                can_move: true,
                can_resize: true,
                can_minimize: true,
                can_maximize: true,
            },
            lifecycle: WindowLifecycle::Active,
        }
    }

    /// A state in which the border *should* show, so each test below can
    /// change exactly the one thing it is about.
    fn tiling_state_with_focused_window(action: ManageAction) -> EngineState {
        let id = WindowId(1);
        // `EngineState` has private fields, so it is built by assignment
        // rather than struct-update syntax from outside `mosaix-engine`.
        let mut state = EngineState::default();
        state.automatic_tiling_active = true;
        state.focused_window = Some(id);
        state.inventory.insert(
            id,
            ManagedWindow {
                window: window(1),
                action,
                eligibility: EligibilityReason::Eligible,
            },
        );
        state.windows.insert(
            id,
            WindowPlacement {
                display_id: DisplayId(1),
                bounds: BOUNDS,
                observed_bounds: BOUNDS,
                previous_placement: None,
                cycle_step: None,
                rejection_count: 0,
            },
        );
        state
    }

    fn tiling_state() -> EngineState {
        tiling_state_with_focused_window(ManageAction::Tile)
    }

    #[test]
    fn outlines_the_focused_window_while_tiling_is_active() {
        let target = focus_border_target(&tiling_state()).expect("border should show");

        assert_eq!(target.bounds, BOUNDS);
        assert_eq!(target.thickness, 3);
        assert_eq!(target.color, Rgb::new(0x00, 0x78, 0xD7));
    }

    #[test]
    fn a_floating_window_still_gets_a_border() {
        let state = tiling_state_with_focused_window(ManageAction::Float);

        assert!(
            focus_border_target(&state).is_some(),
            "CONTEXT.md: the border covers tiled and floating windows alike"
        );
    }

    #[test]
    fn hidden_when_the_config_disables_it() {
        let mut state = tiling_state();
        state.resolved_config.focus_border.enabled = false;

        assert_eq!(focus_border_target(&state), None);
    }

    #[test]
    fn hidden_while_paused() {
        let mut state = tiling_state();
        state.paused = true;

        assert_eq!(focus_border_target(&state), None);
    }

    #[test]
    fn hidden_when_tiling_is_not_active() {
        let mut state = tiling_state();
        // Covers both manual (no tiling profile matched) and suspended --
        // `automatic_tiling_active` is false in both.
        state.automatic_tiling_active = false;

        assert_eq!(focus_border_target(&state), None);
    }

    #[test]
    fn hidden_when_nothing_is_focused_yet() {
        let mut state = tiling_state();
        state.focused_window = None;

        assert_eq!(focus_border_target(&state), None);
    }

    #[test]
    fn hidden_when_the_focused_window_is_not_managed() {
        let mut state = tiling_state();
        state.inventory.clear();

        assert_eq!(
            focus_border_target(&state),
            None,
            "an Exclude window is absent from the inventory and gets no border"
        );
    }

    #[test]
    fn follows_where_the_window_actually_is_not_where_it_was_placed() {
        let mut state = tiling_state();
        let moved_to = Rect::new(640, 480, 300, 250);
        // What the engine records when something other than Mosaix moves a
        // window: `observed_bounds` tracks reality, `bounds` deliberately
        // keeps holding the last intended placement so ADR 0001's
        // correlation still detects the external move.
        let placement = state.windows.get_mut(&WindowId(1)).unwrap();
        placement.observed_bounds = moved_to;

        let target = focus_border_target(&state).expect("border should show");

        assert_eq!(
            target.bounds, moved_to,
            "the border must track the window, not its last intended placement"
        );
    }

    #[test]
    fn hidden_for_a_window_that_has_no_pixels_on_screen() {
        for lifecycle in [
            WindowLifecycle::Minimized,
            WindowLifecycle::Hidden,
            WindowLifecycle::Cloaked,
        ] {
            let mut state = tiling_state();
            state
                .inventory
                .get_mut(&WindowId(1))
                .unwrap()
                .window
                .lifecycle = lifecycle;

            assert_eq!(
                focus_border_target(&state),
                None,
                "{lifecycle:?} stays in the inventory as temporarily ineligible, \
                 so membership alone would leave a border on bare desktop"
            );
        }
    }

    #[test]
    fn still_shown_for_a_maximized_or_fullscreen_window() {
        for lifecycle in [WindowLifecycle::Maximized, WindowLifecycle::Fullscreen] {
            let mut state = tiling_state();
            state
                .inventory
                .get_mut(&WindowId(1))
                .unwrap()
                .window
                .lifecycle = lifecycle;

            assert!(
                focus_border_target(&state).is_some(),
                "{lifecycle:?} is ineligible for a grid cell but still visible"
            );
        }
    }

    #[test]
    fn hidden_while_the_focused_window_is_being_dragged() {
        let mut state = tiling_state();
        state.interactive_placement = Some(InteractivePlacementSession {
            window_id: WindowId(1),
            display_id: DisplayId(1),
        });

        assert_eq!(
            focus_border_target(&state),
            None,
            "the engine defers placement during a drag, so a border drawn \
             from `windows` would trail the window under the cursor"
        );
    }

    #[test]
    fn a_drag_of_some_other_window_does_not_hide_the_border() {
        let mut state = tiling_state();
        state.interactive_placement = Some(InteractivePlacementSession {
            window_id: WindowId(99),
            display_id: DisplayId(1),
        });

        assert!(
            focus_border_target(&state).is_some(),
            "only the dragged window's own border goes stale"
        );
    }

    #[test]
    fn hidden_when_the_engine_has_no_placement_for_the_focused_window() {
        let mut state = tiling_state();
        state.windows.clear();

        assert_eq!(focus_border_target(&state), None);
    }

    #[test]
    fn carries_the_resolved_color_and_thickness() {
        let mut state = tiling_state();
        state.resolved_config.focus_border.color = "#FF8800".to_string();
        state.resolved_config.focus_border.thickness = 7;

        let target = focus_border_target(&state).expect("border should show");

        assert_eq!(target.color, Rgb::new(0xFF, 0x88, 0x00));
        assert_eq!(target.thickness, 7);
    }

    #[test]
    fn falls_back_to_the_default_color_when_the_resolved_one_cannot_parse() {
        let mut state = tiling_state();
        state.resolved_config.focus_border.color = "not-a-color".to_string();

        let target = focus_border_target(&state).expect("border should still show");

        assert_eq!(target.color, Rgb::new(0x00, 0x78, 0xD7));
    }
}
