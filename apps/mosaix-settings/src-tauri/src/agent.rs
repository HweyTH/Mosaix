//! The settings application's link to the authoritative agent.
//!
//! The application asks the agent to perform a change and reports only
//! what the agent confirms; it never performs the change itself and never
//! writes configuration files. The connection is *held* rather than opened
//! per request, because hotkey-capture suspension is bounded by this
//! connection's lifetime, so the agent recovers when the application dies
//! and the operating system closes the pipe handle.

use mosaix_config::{BindingEdit, LayoutEdit};
use mosaix_ipc::StateSnapshot;

/// Why an agent request did not succeed. The three cases are kept apart
/// deliberately: a user running an old agent needs to be told to update,
/// not left reading a rejection reason that will never make sense.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentError {
    /// No agent is listening, or the one that was listening has gone.
    Unavailable,
    /// The agent answered, and refused, in its own words.
    Rejected { reason: String },
    /// The agent speaks a different protocol version.
    VersionMismatch { server_version: u32 },
    /// The connection itself misbehaved.
    Transport { detail: String },
}

/// What the editor session needs from the agent. A trait so the session's
/// own behaviour -- what it reports for a confirmed apply, a rejection,
/// and a version mismatch -- is testable without a running agent.
pub trait AgentTransport: Send + std::fmt::Debug {
    /// Asks the agent to apply the saved layout called `name`. Returns
    /// `Ok` only when the agent confirms it did so.
    fn apply_saved_layout(&mut self, name: &str) -> Result<(), AgentError>;

    /// Reads the agent's current state: the resolved hotkey bindings with
    /// their provenance, the saved layouts on offer, and the topology
    /// those belong to. The agent is the authority for all three -- the
    /// settings application reads no configuration file of its own.
    fn state(&mut self) -> Result<StateSnapshot, AgentError>;

    /// Asks the agent to change the saved-layout set, returning the
    /// configuration file the write landed in.
    ///
    /// The agent performs every configuration write, so this is the only
    /// way the settings application changes a layout, and a change it
    /// cannot confirm is an error rather than a claim.
    fn edit_layouts(&mut self, edit: LayoutEdit) -> Result<String, AgentError>;

    /// Asks the agent to unregister every hotkey, so a combination the
    /// user presses next reaches the editor instead of firing a command.
    ///
    /// Suspension lasts until [`AgentTransport::end_hotkey_capture`] or,
    /// failing that, until this connection ends -- which is what brings
    /// the hotkeys back when the settings application is killed rather
    /// than closed.
    fn start_hotkey_capture(&mut self) -> Result<(), AgentError>;

    /// Asks the agent to register the hotkeys again. The editor closing
    /// cleanly is the ordinary way suspension ends.
    fn end_hotkey_capture(&mut self) -> Result<(), AgentError>;

    /// Asks whether `combo` can be bound, before the user commits to it.
    ///
    /// The agent answers, not this application: only the agent can attempt
    /// the registration, and only it knows Mosaix's own resolved bindings
    /// well enough to say a conflict is one the user can resolve
    /// themselves.
    fn probe_hotkey(
        &mut self,
        combo: &str,
        for_command: &str,
    ) -> Result<serde_json::Value, AgentError>;

    /// Asks the agent to change a binding, returning the configuration
    /// file the write landed in and the combination now in effect.
    fn edit_binding(&mut self, edit: BindingEdit) -> Result<(String, Option<String>), AgentError>;

    /// Asks the agent to put back every window this session parked,
    /// through the verified restore path (issue #63).
    ///
    /// This and the reconcile below are the settings surface's two repair
    /// actions. Both are ordinary agent requests: the settings
    /// application never touches the recovery ledger or the state
    /// database itself, exactly as it never writes a configuration file.
    fn restore_parked_windows(&mut self) -> Result<serde_json::Value, AgentError>;

    /// Asks the agent to reconcile the windows a failed switch left
    /// unaccounted for, which is the only way out of the
    /// workspace-switch-degraded condition.
    fn restore_workspace_switch(&mut self) -> Result<serde_json::Value, AgentError>;
}

/// The transport this build talks to a real agent through.
pub fn connect() -> Box<dyn AgentTransport> {
    #[cfg(windows)]
    {
        Box::new(windows_transport::HeldConnection::opened())
    }
    #[cfg(not(windows))]
    {
        Box::new(UnsupportedPlatform)
    }
}

/// Stands in for the transport on platforms with no agent to talk to, so
/// the settings application still builds and reports the absence honestly.
#[cfg(not(windows))]
#[derive(Debug)]
struct UnsupportedPlatform;

#[cfg(not(windows))]
impl AgentTransport for UnsupportedPlatform {
    fn apply_saved_layout(&mut self, _name: &str) -> Result<(), AgentError> {
        Err(AgentError::Unavailable)
    }

    fn state(&mut self) -> Result<StateSnapshot, AgentError> {
        Err(AgentError::Unavailable)
    }

    fn edit_layouts(&mut self, _edit: LayoutEdit) -> Result<String, AgentError> {
        Err(AgentError::Unavailable)
    }

    fn start_hotkey_capture(&mut self) -> Result<(), AgentError> {
        Err(AgentError::Unavailable)
    }

    fn end_hotkey_capture(&mut self) -> Result<(), AgentError> {
        Err(AgentError::Unavailable)
    }

    fn probe_hotkey(
        &mut self,
        _combo: &str,
        _for_command: &str,
    ) -> Result<serde_json::Value, AgentError> {
        Err(AgentError::Unavailable)
    }

    fn edit_binding(&mut self, _edit: BindingEdit) -> Result<(String, Option<String>), AgentError> {
        Err(AgentError::Unavailable)
    }

    fn restore_parked_windows(&mut self) -> Result<serde_json::Value, AgentError> {
        Err(AgentError::Unavailable)
    }

    fn restore_workspace_switch(&mut self) -> Result<serde_json::Value, AgentError> {
        Err(AgentError::Unavailable)
    }
}

#[cfg(windows)]
mod windows_transport {
    use mosaix_config::{BindingEdit, LayoutEdit};
    use mosaix_ipc::{IpcConnection, IpcError, IpcRequest, IpcResponse, StateSnapshot};

    use super::{AgentError, AgentTransport};

    /// One connection to the agent, opened when the window opens and kept
    /// for its lifetime.
    ///
    /// The connection is `Option` because the agent may not be running
    /// when the window opens, and may exit while it is open. Either way
    /// the next request reconnects rather than reporting a permanent
    /// failure -- starting the agent afterwards is a reasonable thing for
    /// a user to do.
    #[derive(Debug, Default)]
    pub struct HeldConnection {
        connection: Option<IpcConnection>,
        /// Whether the editor believes hotkey capture is in effect.
        ///
        /// Suspension is bounded by the connection, so a connection that
        /// breaks and is replaced takes the agent's suspension with it.
        /// Remembering the intent here is what lets the replacement
        /// re-assert it, rather than leaving the editor showing a capture
        /// dialog while every hotkey is live again.
        capturing: bool,
    }

    impl HeldConnection {
        /// Opens the connection now, so it is held for the window's
        /// lifetime rather than established on first use. A failure here
        /// is not fatal: the agent may simply not be running yet.
        pub fn opened() -> Self {
            match IpcConnection::connect() {
                Ok(connection) => Self {
                    connection: Some(connection),
                    capturing: false,
                },
                Err(error) => {
                    tracing::debug!(%error, "no Mosaix agent to connect to yet");
                    Self::default()
                }
            }
        }

        /// Sends `request` and returns the data the agent answered with,
        /// only when the agent confirmed the request. A refusal, a version
        /// mismatch, and a broken connection stay three distinct errors.
        fn confirmed(
            &mut self,
            request: IpcRequest,
        ) -> Result<Option<serde_json::Value>, AgentError> {
            match self.send(request)? {
                IpcResponse::Ok { data } => Ok(data),
                IpcResponse::Error { message } => Err(AgentError::Rejected { reason: message }),
                IpcResponse::VersionMismatch { server_version } => {
                    Err(AgentError::VersionMismatch { server_version })
                }
            }
        }

        fn send(&mut self, request: IpcRequest) -> Result<IpcResponse, AgentError> {
            if self.connection.is_none() {
                let mut connection = IpcConnection::connect().map_err(from_ipc_error)?;
                if self.capturing && request != IpcRequest::StartHotkeyCapture {
                    // The agent released suspension when the previous
                    // connection died. Re-asserting it before anything
                    // else keeps the editor's claim true.
                    let _ = connection.send(IpcRequest::StartHotkeyCapture);
                }
                self.connection = Some(connection);
            }
            let outcome = self
                .connection
                .as_mut()
                .expect("just connected")
                .send(request);
            if outcome.is_err() {
                // A broken connection stays broken; drop it so the next
                // request opens a fresh one instead of writing into a
                // handle the agent has already closed.
                self.connection = None;
            }
            outcome.map_err(from_ipc_error)
        }
    }

    impl AgentTransport for HeldConnection {
        fn apply_saved_layout(&mut self, name: &str) -> Result<(), AgentError> {
            self.confirmed(IpcRequest::ApplyLayout {
                name: name.to_owned(),
            })
            .map(|_| ())
        }

        fn edit_layouts(&mut self, edit: LayoutEdit) -> Result<String, AgentError> {
            let data = self.confirmed(match edit {
                LayoutEdit::Save {
                    name,
                    cells,
                    to_base,
                } => IpcRequest::SaveLayout {
                    name,
                    cells,
                    to_base,
                },
                LayoutEdit::Rename { from, to } => IpcRequest::RenameLayout { from, to },
                LayoutEdit::Duplicate { from, to } => IpcRequest::DuplicateLayout { from, to },
                LayoutEdit::Delete { name } => IpcRequest::DeleteLayout { name },
            })?;
            Ok(data
                .as_ref()
                .and_then(|data| data.get("file"))
                .and_then(|file| file.as_str())
                .unwrap_or("your configuration")
                .to_owned())
        }

        fn start_hotkey_capture(&mut self) -> Result<(), AgentError> {
            self.confirmed(IpcRequest::StartHotkeyCapture)?;
            self.capturing = true;
            Ok(())
        }

        fn end_hotkey_capture(&mut self) -> Result<(), AgentError> {
            // Cleared first: whatever the agent answers, this editor is
            // no longer capturing, and a reconnect must not revive a
            // suspension the user has finished with.
            self.capturing = false;
            self.confirmed(IpcRequest::EndHotkeyCapture).map(|_| ())
        }

        fn probe_hotkey(
            &mut self,
            combo: &str,
            for_command: &str,
        ) -> Result<serde_json::Value, AgentError> {
            Ok(self
                .confirmed(IpcRequest::ProbeHotkey {
                    combo: combo.to_owned(),
                    for_command: Some(for_command.to_owned()),
                })?
                .unwrap_or(serde_json::Value::Null))
        }

        fn edit_binding(
            &mut self,
            edit: BindingEdit,
        ) -> Result<(String, Option<String>), AgentError> {
            let data = self.confirmed(match edit {
                BindingEdit::Set {
                    command,
                    combo,
                    to_base,
                } => IpcRequest::SetBinding {
                    command_path: command.to_string(),
                    combo: combo.to_string(),
                    to_base,
                },
                BindingEdit::Reset { command } => IpcRequest::ResetBinding {
                    command_path: command.to_string(),
                },
            })?;
            let field = |name: &str| {
                data.as_ref()
                    .and_then(|data| data.get(name))
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
            };
            Ok((
                field("file").unwrap_or_else(|| "your configuration".to_owned()),
                field("combo"),
            ))
        }

        fn state(&mut self) -> Result<StateSnapshot, AgentError> {
            let data = self.confirmed(IpcRequest::GetState)?;
            serde_json::from_value(data.unwrap_or(serde_json::Value::Null)).map_err(|error| {
                // The agent answered, but not with the shape this build
                // expects. That is a transport-level disagreement, not a
                // refusal, and a version mismatch would already have been
                // reported as one.
                AgentError::Transport {
                    detail: format!("could not read the agent's state: {error}"),
                }
            })
        }

        fn restore_parked_windows(&mut self) -> Result<serde_json::Value, AgentError> {
            Ok(self
                .confirmed(IpcRequest::RestoreParkedWindows)?
                .unwrap_or(serde_json::Value::Null))
        }

        fn restore_workspace_switch(&mut self) -> Result<serde_json::Value, AgentError> {
            Ok(self
                .confirmed(IpcRequest::RestoreWorkspaceSwitch)?
                .unwrap_or(serde_json::Value::Null))
        }
    }

    fn from_ipc_error(error: IpcError) -> AgentError {
        match error {
            IpcError::ConnectionFailed | IpcError::ConnectionClosed => AgentError::Unavailable,
            IpcError::VersionMismatch { server_version } => {
                AgentError::VersionMismatch { server_version }
            }
            other => AgentError::Transport {
                detail: other.to_string(),
            },
        }
    }
}
