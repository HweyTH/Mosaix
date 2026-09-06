/**
 * The experimental workspace-switching surface (issue #63).
 *
 * Read-only apart from the two repair buttons. Activation is profile-only
 * (spec #45 user story 71): base configuration can never request
 * switching, so this panel deliberately offers no control that would turn
 * it on. It shows which profile asks for it, whether that profile's
 * mapping covers every connected display, whether the adapter has
 * verified a parking site, and what repair is waiting on a person.
 *
 * The wording is the other half of its job. Spec #45 user story 97 asks
 * for the feature to be described honestly wherever it appears, so this
 * panel says "emulated" and never "virtual desktops" or "Spaces".
 */

export interface WorkspaceMapping {
  display: string;
  workspace: string;
}

export interface WorkspaceSummary {
  name: string;
  origin: string;
  displayedOn: number | null;
  memberCount: number;
}

export interface RecoveryAction {
  reason: string;
  windows: number[];
  command: string;
}

export interface WorkspaceStatus {
  topologyFingerprint: string;
  /** `disabled`, `requested`, `unavailable`, or `experimental`. */
  switchingStatus: string;
  switchingReason: string | null;
  profileFile: string | null;
  mapping: WorkspaceMapping[];
  mappingComplete: boolean;
  displayCount: number;
  /** `unverified`, `verified`, or `refused`. */
  parkingCapability: string;
  parkingCapabilityReason: string | null;
  workspaces: WorkspaceSummary[];
  recoveryRequired: boolean;
  recoveryActions: RecoveryAction[];
  parkedWindows: number[];
}

/** What a repair action did, in the agent's own account of it. */
export interface RepairReceipt {
  answer: unknown;
}

export interface WorkspaceBridge {
  loadWorkspaceStatus(): Promise<WorkspaceStatus>;
  restoreParkedWindows(): Promise<RepairReceipt>;
  restoreWorkspaceSwitch(): Promise<RepairReceipt>;
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

/**
 * One sentence saying what switching is doing and why, in the same four
 * states the engine publishes.
 *
 * `disabled` names the reason it cannot be turned on here, because a user
 * looking for a switch that is not on this panel deserves to be told
 * where it lives rather than left hunting.
 */
export function switchingSentence(status: WorkspaceStatus): string {
  switch (status.switchingStatus) {
    case "disabled":
      return "Disabled. Only a matched topology profile can request switching; base configuration cannot.";
    case "requested":
      return `Requested by a profile, not yet active${
        status.switchingReason === null ? "" : ` (${status.switchingReason})`
      }.`;
    case "unavailable":
      return `Unavailable; the previous displayed assignment stands${
        status.switchingReason === null ? "" : ` (${status.switchingReason})`
      }.`;
    case "experimental":
      return "Experimental and active.";
    default:
      return status.switchingStatus;
  }
}

/**
 * How the parking mechanism is described to the user.
 *
 * Never "virtual desktops" or "Spaces": the mechanism is window
 * relocation through public APIs, and calling it the native feature would
 * promise behaviour it does not have (spec #45 user story 97).
 */
export function parkingSentence(status: WorkspaceStatus): string {
  const mechanism =
    "Windows are moved to a recoverable off-screen position using public APIs. This is emulated switching, not native virtual desktops or Spaces.";
  switch (status.parkingCapability) {
    case "verified":
      return `Parking site verified. ${mechanism}`;
    case "unverified":
      return `Parking site not verified yet; switching waits for it. ${mechanism}`;
    case "refused":
      return `No recoverable parking site${
        status.parkingCapabilityReason === null
          ? ""
          : `: ${status.parkingCapabilityReason}`
      }. Switching is refused rather than falling back to another way of hiding windows.`;
    default:
      return mechanism;
  }
}

/** The plain-English description of one outstanding repair. */
export function repairSentence(action: RecoveryAction): string {
  switch (action.reason) {
    case "switch_degraded":
      return "A failed switch left windows unaccounted for. Switching stays blocked until they are reconciled.";
    case "parking_restore_failed":
      return "A window could not be put back and is still parked off screen.";
    case "startup_recovery_incomplete":
      return "A previous session left windows parked that startup could not verify and put back.";
    case "persistence_degraded":
      return "Nothing new is becoming durable, so parking and new undo entries are unavailable.";
    default:
      return action.reason;
  }
}

function renderMapping(status: WorkspaceStatus): string {
  if (status.mapping.length === 0) {
    return `<p data-mapping-empty>No profile declares a workspace mapping for this topology.</p>`;
  }
  const completeness = status.mappingComplete
    ? `<p class="mapping-complete" data-mapping-complete>Every one of the ${status.displayCount} connected display(s) is mapped.</p>`
    : `<p class="mapping-incomplete" data-mapping-incomplete>Incomplete: ${status.mapping.length} of ${status.displayCount} connected display(s) mapped. A mapping applies to every display or to none.</p>`;
  const rows = status.mapping
    .map(
      (entry) =>
        `<li data-mapping-row><code>${escapeHtml(entry.display)}</code> → <b>${escapeHtml(entry.workspace)}</b></li>`,
    )
    .join("");
  return `${completeness}<ul class="mapping-list">${rows}</ul>`;
}

function renderWorkspaces(status: WorkspaceStatus): string {
  if (status.workspaces.length === 0) return `<p>No workspaces.</p>`;
  const rows = status.workspaces
    .map((workspace) => {
      const place =
        workspace.displayedOn === null
          ? "hidden"
          : `display ${workspace.displayedOn}`;
      return `<li data-workspace="${escapeHtml(workspace.name)}"><b>${escapeHtml(workspace.name)}</b> <small>${place}, from ${escapeHtml(workspace.origin)}, ${workspace.memberCount} window(s)</small></li>`;
    })
    .join("");
  return `<ul class="workspace-list">${rows}</ul>`;
}

function renderRepairs(status: WorkspaceStatus): string {
  const buttons = `
    <div class="repair-actions">
      <button class="soft-button" data-restore-switch ${
        status.recoveryActions.some((action) => action.reason === "switch_degraded")
          ? ""
          : "disabled"
      }>Reconcile failed switch</button>
      <button class="soft-button" data-restore-parked ${
        status.parkedWindows.length === 0 &&
        !status.recoveryActions.some(
          (action) =>
            action.reason === "parking_restore_failed" ||
            action.reason === "startup_recovery_incomplete",
        )
          ? "disabled"
          : ""
      }>Put parked windows back</button>
    </div>`;
  if (!status.recoveryRequired) {
    return `<p data-recovery-clear>Nothing is waiting on you.</p>${buttons}`;
  }
  const rows = status.recoveryActions
    .map(
      (action) =>
        `<li data-repair="${escapeHtml(action.reason)}">${escapeHtml(repairSentence(action))}${
          action.windows.length === 0
            ? ""
            : `<br><small>Windows ${action.windows.join(" ")}</small>`
        }<br><small>Or run <code>${escapeHtml(action.command)}</code></small></li>`,
    )
    .join("");
  return `<ul class="repair-list" data-recovery-required>${rows}</ul>${buttons}`;
}

/**
 * The whole panel, as HTML.
 *
 * A pure function of the status so the wording -- which is most of what
 * this panel is -- is testable without a running agent, matching how the
 * CLI's own status report is built and tested.
 */
export function renderWorkspaceStatus(
  status: WorkspaceStatus | undefined,
  error: string | undefined,
  repairStatus: string | undefined,
): string {
  if (error !== undefined) {
    return `<p data-workspace-error>${escapeHtml(error)}</p>`;
  }
  if (status === undefined) return `<p>Reading workspace status…</p>`;
  return `
    <p><small>Current topology</small><br><code data-workspace-topology>${escapeHtml(status.topologyFingerprint)}</code></p>
    <p data-switching-status>${escapeHtml(switchingSentence(status))}</p>
    ${
      status.profileFile === null
        ? ""
        : `<p><small>Requested by</small> <code data-profile-file>${escapeHtml(status.profileFile)}</code></p>`
    }
    <div class="panel-rule"></div>
    <div class="panel-title">MAPPING</div>
    ${renderMapping(status)}
    <div class="panel-rule"></div>
    <div class="panel-title">WORKSPACES</div>
    ${renderWorkspaces(status)}
    <div class="panel-rule"></div>
    <div class="panel-title">PARKING</div>
    <p data-parking-capability>${escapeHtml(parkingSentence(status))}</p>
    <div class="panel-rule"></div>
    <div class="panel-title">RECOVERY</div>
    ${renderRepairs(status)}
    ${repairStatus === undefined ? "" : `<p class="repair-status" role="status" data-repair-status>${escapeHtml(repairStatus)}</p>`}`;
}

/**
 * Wires the panel's two repair buttons to the bridge.
 *
 * Both are ordinary agent requests. A repair the agent does not confirm
 * is reported as a failure rather than as a change that happened, the
 * same rule the layout writes follow (ADR 0022).
 */
export function bindWorkspaceRepairs(
  root: HTMLElement,
  bridge: WorkspaceBridge,
  onDone: (message: string) => void,
): void {
  const run = async (
    repair: () => Promise<RepairReceipt>,
    describe: string,
  ): Promise<void> => {
    onDone(`${describe}…`);
    try {
      await repair();
      onDone(`${describe}: the agent confirmed it.`);
    } catch (error: unknown) {
      onDone(
        `${describe} failed: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  };

  root
    .querySelector<HTMLButtonElement>("[data-restore-switch]")
    ?.addEventListener("click", () => {
      void run(
        () => bridge.restoreWorkspaceSwitch(),
        "Reconciling the failed switch",
      );
    });
  root
    .querySelector<HTMLButtonElement>("[data-restore-parked]")
    ?.addEventListener("click", () => {
      void run(
        () => bridge.restoreParkedWindows(),
        "Putting parked windows back",
      );
    });
}
