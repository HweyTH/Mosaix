use crate::Result;
use mosaix_domain::Rect;

/// Handle for the click-through snap preview overlay.
pub struct PreviewOverlay;
impl PreviewOverlay {
    pub fn show(&self, _bounds: Rect) {}
    pub fn hide(&self) {}
    pub fn stop(self) {}
}
pub fn start_preview_overlay() -> Result<PreviewOverlay> {
    Ok(PreviewOverlay)
}
