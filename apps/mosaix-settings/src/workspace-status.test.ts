import { describe, expect, it, vi } from "vitest";

import {
  bindWorkspaceRepairs,
  parkingSentence,
  renderWorkspaceStatus,
  repairSentence,
  switchingSentence,
  type WorkspaceBridge,
  type WorkspaceStatus,
} from "./workspace-status";

function status(overrides: Partial<WorkspaceStatus> = {}): WorkspaceStatus {
  return {
    topologyFingerprint: "MON-A+MON-B",
    switchingStatus: "experimental",
    switchingReason: null,
    profileFile: "office.toml",
    mapping: [
      { display: "MON-A", workspace: "dev" },
      { display: "MON-B", workspace: "chat" },
    ],
    mappingComplete: true,
    displayCount: 2,
    parkingCapability: "verified",
    parkingCapabilityReason: null,
    workspaces: [
      { name: "dev", origin: "configuration", displayedOn: 1, memberCount: 3 },
      { name: "chat", origin: "command", displayedOn: null, memberCount: 0 },
    ],
    recoveryRequired: false,
    recoveryActions: [],
    parkedWindows: [],
    ...overrides,
  };
}

function render(value: WorkspaceStatus): HTMLElement {
  const root = document.createElement("div");
  root.innerHTML = renderWorkspaceStatus(value, undefined, undefined);
  return root;
}

describe("activation", () => {
  it("says only a profile can request switching, and names the one that does", () => {
    const root = render(status());

    expect(root.querySelector("[data-profile-file]")?.textContent).toBe("office.toml");
    expect(root.querySelector("[data-switching-status]")?.textContent).toBe(
      "Experimental and active.",
    );
  });

  it("points a user looking for a switch at the profile rather than offering one", () => {
    // The panel must never imply the settings window can enable the
    // experiment: base configuration cannot request it at all.
    const root = render(status({ switchingStatus: "disabled", profileFile: null }));

    expect(root.querySelector("[data-switching-status]")?.textContent).toContain(
      "Only a matched topology profile can request switching; base configuration cannot.",
    );
    expect(root.querySelector("input[type=checkbox]")).toBeNull();
    expect(root.querySelector("[data-profile-file]")).toBeNull();
  });

  it("keeps the reason a requested mapping has not activated", () => {
    expect(
      switchingSentence(
        status({
          switchingStatus: "requested",
          switchingReason: "parking_capability_unverified",
        }),
      ),
    ).toBe("Requested by a profile, not yet active (parking_capability_unverified).");
  });
});

describe("mapping", () => {
  it("shows the complete workspace-to-monitor mapping", () => {
    const root = render(status());

    const rows = [...root.querySelectorAll("[data-mapping-row]")].map(
      (row) => row.textContent,
    );
    expect(rows).toEqual(["MON-A → dev", "MON-B → chat"]);
    expect(root.querySelector("[data-mapping-complete]")).not.toBeNull();
  });

  it("says a mapping that misses a display is incomplete and all-or-nothing", () => {
    const root = render(status({ mappingComplete: false, displayCount: 3 }));

    const notice = root.querySelector("[data-mapping-incomplete]")?.textContent;
    expect(notice).toContain("2 of 3 connected display(s) mapped");
    expect(notice).toContain("every display or to none");
  });
});

describe("capability", () => {
  it("never calls parking a native virtual desktop or Space", () => {
    for (const capability of ["verified", "unverified", "refused"]) {
      const sentence = parkingSentence(status({ parkingCapability: capability }));
      expect(sentence.toLowerCase()).not.toContain("native virtual desktops or spaces.");
      expect(sentence).toMatch(/emulated|refused/i);
    }
  });

  it("gives the adapter's reason for a refused site, and says nothing else is tried", () => {
    const sentence = parkingSentence(
      status({
        parkingCapability: "refused",
        parkingCapabilityReason: "no recoverable site beyond the virtual screen",
      }),
    );

    expect(sentence).toContain("no recoverable site beyond the virtual screen");
    expect(sentence).toContain("rather than falling back to another way of hiding windows");
  });
});

describe("recovery", () => {
  it("reports a clear state without inventing a repair", () => {
    const root = render(status());

    expect(root.querySelector("[data-recovery-clear]")?.textContent).toBe(
      "Nothing is waiting on you.",
    );
    expect(
      root.querySelector<HTMLButtonElement>("[data-restore-switch]")?.disabled,
    ).toBe(true);
  });

  it("names each outstanding repair, its windows, and the command behind it", () => {
    const root = render(
      status({
        recoveryRequired: true,
        recoveryActions: [
          {
            reason: "switch_degraded",
            windows: [41, 42],
            command: "mosaix workspace restore-switch",
          },
        ],
      }),
    );

    const repair = root.querySelector('[data-repair="switch_degraded"]');
    expect(repair?.textContent).toContain("Switching stays blocked");
    expect(repair?.textContent).toContain("Windows 41 42");
    expect(repair?.textContent).toContain("mosaix workspace restore-switch");
    expect(
      root.querySelector<HTMLButtonElement>("[data-restore-switch]")?.disabled,
    ).toBe(false);
  });

  it("offers to put parked windows back whenever any are parked", () => {
    const root = render(status({ parkedWindows: [7] }));

    expect(
      root.querySelector<HTMLButtonElement>("[data-restore-parked]")?.disabled,
    ).toBe(false);
  });

  it("describes every repair reason the agent can publish", () => {
    for (const reason of [
      "switch_degraded",
      "parking_restore_failed",
      "startup_recovery_incomplete",
      "persistence_degraded",
    ]) {
      const sentence = repairSentence({ reason, windows: [], command: "x" });
      expect(sentence).not.toBe(reason);
    }
  });
});

describe("repair actions", () => {
  it("asks the agent and reports what it confirmed", async () => {
    const root = render(status({ parkedWindows: [7] }));
    const bridge: WorkspaceBridge = {
      loadWorkspaceStatus: vi.fn(),
      restoreParkedWindows: vi.fn().mockResolvedValue({ answer: {} }),
      restoreWorkspaceSwitch: vi.fn(),
    };
    const messages: string[] = [];
    bindWorkspaceRepairs(root, bridge, (message) => messages.push(message));

    root.querySelector<HTMLButtonElement>("[data-restore-parked]")?.click();
    await vi.waitFor(() => expect(messages.length).toBe(2));

    expect(bridge.restoreParkedWindows).toHaveBeenCalledOnce();
    expect(messages.at(-1)).toBe("Putting parked windows back: the agent confirmed it.");
  });

  it("reports a repair the agent refused as a failure, not as a change", async () => {
    const root = render(
      status({
        recoveryRequired: true,
        recoveryActions: [
          {
            reason: "switch_degraded",
            windows: [41],
            command: "mosaix workspace restore-switch",
          },
        ],
      }),
    );
    const bridge: WorkspaceBridge = {
      loadWorkspaceStatus: vi.fn(),
      restoreParkedWindows: vi.fn(),
      restoreWorkspaceSwitch: vi
        .fn()
        .mockRejectedValue(new Error("no agent is running")),
    };
    const messages: string[] = [];
    bindWorkspaceRepairs(root, bridge, (message) => messages.push(message));

    root.querySelector<HTMLButtonElement>("[data-restore-switch]")?.click();
    await vi.waitFor(() => expect(messages.length).toBe(2));

    expect(messages.at(-1)).toBe(
      "Reconciling the failed switch failed: no agent is running",
    );
  });
});

describe("failure to read", () => {
  it("says the status could not be read rather than showing a healthy panel", () => {
    const root = document.createElement("div");
    root.innerHTML = renderWorkspaceStatus(undefined, "no agent is running", undefined);

    expect(root.querySelector("[data-workspace-error]")?.textContent).toBe(
      "no agent is running",
    );
    expect(root.querySelector("[data-recovery-clear]")).toBeNull();
  });
});
