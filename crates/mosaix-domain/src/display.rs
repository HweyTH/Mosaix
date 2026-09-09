//! Display topology types: [`Display`], [`Rotation`], and
//! [`topology_fingerprint`], the identity helper platform adapters use to
//! tell a genuine topology change from a spurious re-enumeration.

use serde::{Deserialize, Serialize};

use crate::geometry::Rect;
use crate::id::DisplayId;

/// Physical display rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rotation {
    Landscape,
    Portrait,
    LandscapeFlipped,
    PortraitFlipped,
}

/// A monitor's geometry, scale, and identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Display {
    pub id: DisplayId,
    pub stable_fingerprint: String,
    pub full_bounds: Rect,
    pub work_area: Rect,
    /// DPI scale relative to 96 DPI (1.0 == 100%, 1.5 == 150%, ...).
    pub scale_factor: f64,
    pub rotation: Rotation,
    pub is_primary: bool,
}

/// A stable-ish fingerprint for a whole display topology: the sorted set
/// of active display fingerprints plus their geometry and scale. Two calls
/// return the same string if and only if the same displays, in the same
/// arrangement and scale, are active -- use this to detect a genuine
/// topology change versus a spurious re-enumeration. Deliberately ignores
/// `id` (ephemeral) and `work_area` (can shift, e.g. taskbar auto-hide,
/// without the topology itself changing).
pub fn topology_fingerprint(displays: &[Display]) -> String {
    let mut parts: Vec<String> = displays
        .iter()
        .map(|d| {
            format!(
                "{}@{},{} {}x{} scale={}",
                d.stable_fingerprint,
                d.full_bounds.x,
                d.full_bounds.y,
                d.full_bounds.width,
                d.full_bounds.height,
                d.scale_factor,
            )
        })
        .collect();
    parts.sort();
    parts.join("|")
}

/// The stable fingerprint of each display in a topology fingerprint, in
/// the order [`topology_fingerprint`] wrote them.
///
/// The inverse of [`topology_fingerprint`], kept beside it so the two
/// cannot drift: a profile maps workspaces to displays by stable
/// fingerprint, and validation has to know which displays a profile's
/// topology holds without a live enumeration. A stable fingerprint may
/// itself contain the `|` that separates entries and the `@` that
/// separates it from its geometry, so entries are recognised by the
/// geometry suffix each one ends with rather than by splitting alone.
pub fn display_fingerprints(topology: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut pending: Vec<&str> = Vec::new();
    for part in topology.split('|') {
        pending.push(part);
        let ends_entry = part
            .rsplit_once('@')
            .is_some_and(|(_, geometry)| is_geometry(geometry));
        if ends_entry {
            let joined = pending.join("|");
            let (stable, _) = joined.rsplit_once('@').expect("checked above");
            entries.push(stable.to_owned());
            pending.clear();
        }
    }
    entries
}

/// Whether `text` is exactly the `x,y wxh scale=s` geometry suffix
/// [`topology_fingerprint`] writes.
fn is_geometry(text: &str) -> bool {
    fn parse(text: &str) -> Option<()> {
        let (position, rest) = text.split_once(' ')?;
        let (x, y) = position.split_once(',')?;
        x.parse::<i32>().ok()?;
        y.parse::<i32>().ok()?;
        let (size, scale) = rest.split_once(' ')?;
        let (width, height) = size.split_once('x')?;
        width.parse::<i32>().ok()?;
        height.parse::<i32>().ok()?;
        scale.strip_prefix("scale=")?.parse::<f64>().ok()?;
        Some(())
    }
    parse(text).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(id: isize, fingerprint: &str, bounds: Rect, scale: f64, primary: bool) -> Display {
        Display {
            id: DisplayId(id),
            stable_fingerprint: fingerprint.to_string(),
            full_bounds: bounds,
            work_area: bounds,
            scale_factor: scale,
            rotation: Rotation::Landscape,
            is_primary: primary,
        }
    }

    const MONITOR_A: Rect = Rect::new(0, 0, 1920, 1080);
    const MONITOR_B: Rect = Rect::new(1920, 0, 1920, 1080);

    #[test]
    fn topology_fingerprint_is_order_independent() {
        let a = display(1, "MON-A", MONITOR_A, 1.0, true);
        let b = display(2, "MON-B", MONITOR_B, 1.25, false);

        assert_eq!(
            topology_fingerprint(&[a.clone(), b.clone()]),
            topology_fingerprint(&[b, a])
        );
    }

    #[test]
    fn topology_fingerprint_changes_when_geometry_changes() {
        let a = display(1, "MON-A", MONITOR_A, 1.0, true);
        let mut moved = a.clone();
        moved.full_bounds.x = 100;

        assert_ne!(topology_fingerprint(&[a]), topology_fingerprint(&[moved]));
    }

    #[test]
    fn topology_fingerprint_changes_when_scale_changes() {
        let a = display(1, "MON-A", MONITOR_A, 1.0, true);
        let mut rescaled = a.clone();
        rescaled.scale_factor = 1.5;

        assert_ne!(
            topology_fingerprint(&[a]),
            topology_fingerprint(&[rescaled])
        );
    }

    #[test]
    fn topology_fingerprint_ignores_display_id_and_work_area() {
        let mut a = display(1, "MON-A", MONITOR_A, 1.0, true);
        a.work_area = Rect::new(0, 40, 1920, 1000);
        let mut b = a.clone();
        b.id = DisplayId(999);
        b.work_area = Rect::new(0, 0, 1920, 1080);

        assert_eq!(topology_fingerprint(&[a]), topology_fingerprint(&[b]));
    }

    #[test]
    fn display_fingerprints_recover_each_stable_fingerprint_even_when_it_contains_the_separators() {
        // The Windows adapter's stable fingerprints contain `|`, and a
        // future one might contain `@`; neither may confuse the parse.
        let a = display(1, r"\\.\DISPLAY1|1920x1080|scale=1", MONITOR_A, 1.0, true);
        let b = display(2, "panel@dock|2560x1440|scale=1.25", MONITOR_B, 1.25, false);

        let topology = topology_fingerprint(&[a.clone(), b.clone()]);
        let mut recovered = display_fingerprints(&topology);
        recovered.sort();
        let mut expected = vec![a.stable_fingerprint, b.stable_fingerprint];
        expected.sort();

        assert_eq!(recovered, expected);
    }

    #[test]
    fn display_fingerprints_of_an_empty_topology_is_empty() {
        assert_eq!(display_fingerprints(""), Vec::<String>::new());
    }
}
