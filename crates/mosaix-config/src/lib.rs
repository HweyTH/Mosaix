//! Configuration schema, validation, atomic reload, and migrations.
//!
//! Pure schema/validation logic (TOML types for base config and profile
//! overlays, field-level merge, and a whole-directory `validate`, ADR 0007)
//! lives in [`validate`] and [`schema`], entirely I/O-free. [`io`] is the
//! thin shell around it: reading a config directory off disk, writing the
//! first-run default, and a `notify`-based watcher that debounces raw
//! filesystem events into validated [`io::ConfigEvent`]s (ADR 0008).
//! Translating those into `Event::ConfigChanged` and feeding them into
//! `mosaix-engine`'s reducer (ADR 0005) is `mosaix-agent`'s job, the same
//! way it already forwards `DisplayTopologyChanged`. [`diff`] is the other
//! half of that ticket's work: a pure diff over resolved hotkey bindings
//! that `mosaix-agent`'s hotkey-rebind poller uses the same way its
//! placement-executor poller already uses `mosaix_engine::diff_placements`.
//!
//! Deliberately has no dependency on `mosaix-engine` or any
//! `mosaix-platform-*` crate -- `mosaix-config` is meant to be shared
//! across platforms and is the thing `mosaix-engine` depends on for
//! `ResolvedConfig`, not the other way around (ADR 0005).

mod defaults;
mod diff;
mod io;
mod schema;
mod validate;

pub use defaults::{default_base_config, default_config_content, fallback_config};
pub use diff::diff_bindings;
pub use io::{
    edit_layouts, ensure_default_config, load, save_profile_settings, watch, ConfigEvent,
    ConfigIoError, ConfigWatcher, LayoutEdit, LayoutEditError, LayoutWrite, ProfileSettingsUpdate,
    DEBOUNCE_WINDOW,
};
pub use schema::{
    AutomaticTilingSection, BaseConfig, BehaviorSection, Command, ConfigLayer, FocusBorderOverride,
    FocusBorderSection, GapsOverride, KeyCombo, ProfileConfig, ResolvedConfig, ResolvedConfigSet,
    ResolvedProfile, RgbaColor, SavedLayout, BASE_CONFIG_FILE_NAME, CURRENT_VERSION,
};
pub use validate::{merge, validate, CandidateConfig, CandidateProfile, ValidationError};
