import { describe, expect, it } from "vitest";

import {
  blurCapture,
  capturedCombo,
  capturedLabel,
  focusCapture,
  isBareKey,
  keyName,
  openCapture,
  pressKey,
  releaseKey,
  type CaptureKeyEvent,
} from "./hotkey-capture";

const nothingHeld = {
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
};

function press(code: string, held: Partial<CaptureKeyEvent> = {}): CaptureKeyEvent {
  return { ...nothingHeld, ...held, code };
}

describe("key names", () => {
  it("reads the physical key rather than the character a layout produces", () => {
    expect(keyName("KeyJ")).toBe("J");
    expect(keyName("Digit1")).toBe("1");
    expect(keyName("ArrowLeft")).toBe("LEFT");
    expect(keyName("PageDown")).toBe("PAGEDOWN");
    expect(keyName("F12")).toBe("F12");
  });

  it("has no name for a key Mosaix cannot bind", () => {
    expect(keyName("Pause")).toBeNull();
    expect(keyName("F25")).toBeNull();
  });
});

describe("capture buffer", () => {
  it("records the combination pressed", () => {
    let session = openCapture(nothingHeld);

    session = pressKey(session, press("KeyJ", { ctrlKey: true, altKey: true }));

    expect(capturedCombo(session)).toBe("ctrl+alt+j");
  });

  it("ignores a modifier press on its own, since it is not a combination", () => {
    let session = openCapture(nothingHeld);

    session = pressKey(session, press("ControlLeft", { ctrlKey: true }));

    expect(capturedCombo(session)).toBeNull();
    expect(capturedLabel(session)).toBe("Press a combination");
  });

  it("accepts a bare key, which RegisterHotKey takes, but flags it", () => {
    let session = openCapture(nothingHeld);

    session = pressKey(session, press("F13"));

    expect(capturedCombo(session)).toBe("f13");
    expect(
      isBareKey(session),
      "a global binding on a bare key takes it from every other application",
    ).toBe(true);
  });

  it("does not flag a combination that carries a modifier", () => {
    let session = openCapture(nothingHeld);

    session = pressKey(session, press("KeyJ", { ctrlKey: true }));

    expect(isBareKey(session)).toBe(false);
  });

  it("says so when Mosaix has no name for the key pressed", () => {
    let session = openCapture(nothingHeld);

    session = pressKey(session, press("Pause", { ctrlKey: true }));

    expect(capturedCombo(session)).toBeNull();
    expect(capturedLabel(session)).toBe("Unsupported key");
  });

  it("replaces the previous capture rather than accumulating", () => {
    let session = openCapture(nothingHeld);
    session = pressKey(session, press("KeyJ", { ctrlKey: true, altKey: true }));

    session = pressKey(session, press("ArrowLeft", { ctrlKey: true }));

    expect(capturedCombo(session)).toBe("ctrl+left");
  });
});

describe("modifier snapshot", () => {
  it("ignores a modifier that was already down when the dialog opened", () => {
    // The dialog was opened while Ctrl was held, so the key-down it
    // consumed left Ctrl reported as down with nothing to release it.
    let session = openCapture({ ...nothingHeld, ctrlKey: true });

    session = pressKey(session, press("KeyJ", { ctrlKey: true, altKey: true }));

    expect(capturedCombo(session)).toBe(
      "alt+j",
      // Without the snapshot this reads ctrl+alt+j and the user has no
      // way to get rid of the ctrl.
    );
  });

  it("counts that modifier again once it has been released and pressed afresh", () => {
    let session = openCapture({ ...nothingHeld, ctrlKey: true });

    session = releaseKey(session, press("ControlLeft"));
    session = pressKey(session, press("KeyJ", { ctrlKey: true, altKey: true }));

    expect(capturedCombo(session)).toBe("ctrl+alt+j");
  });

  it("leaves a modifier the user genuinely pressed alone", () => {
    let session = openCapture(nothingHeld);

    session = pressKey(session, press("KeyJ", { ctrlKey: true }));

    expect(capturedCombo(session)).toBe("ctrl+j");
  });
});

describe("arming", () => {
  it("captures nothing while the dialog is not frontmost", () => {
    let session = blurCapture(openCapture(nothingHeld));

    session = pressKey(session, press("KeyJ", { ctrlKey: true, altKey: true }));

    expect(capturedCombo(session)).toBeNull();
  });

  it("throws away what it held when the dialog loses foreground", () => {
    let session = openCapture(nothingHeld);
    session = pressKey(session, press("KeyJ", { ctrlKey: true, altKey: true }));

    session = blurCapture(session);

    expect(capturedCombo(session)).toBeNull();
    expect(session.armed).toBe(false);
  });

  it("captures again once the dialog is frontmost", () => {
    let session = focusCapture(blurCapture(openCapture(nothingHeld)));

    session = pressKey(session, press("KeyJ", { ctrlKey: true, altKey: true }));

    expect(capturedCombo(session)).toBe("ctrl+alt+j");
  });
});
