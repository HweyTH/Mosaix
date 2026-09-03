use crate::Result;
use std::sync::mpsc::{self, Receiver};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuBarEvent {
    TogglePause,
    OpenConfig,
    Quit,
}
pub struct MenuBarHandle;
impl MenuBarHandle {
    pub fn set_paused(&self, _paused: bool) {}
    pub fn stop(self) {}
}
pub fn start_menu_bar() -> Result<(MenuBarHandle, Receiver<MenuBarEvent>)> {
    let (_tx, rx) = mpsc::channel();
    Ok((MenuBarHandle, rx))
}
