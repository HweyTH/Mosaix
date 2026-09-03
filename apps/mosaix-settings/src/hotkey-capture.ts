/**
 * The capture buffer behind the "press your combination" dialog: what a
 * keypress does to it, when it is armed, and what it holds afterwards.
 *
 * Pure and separate from the dialog's markup, because the rules here are
 * the fiddly part -- arming, blur, and the modifier snapshot -- and they
 * are worth asserting without a DOM in the way.
 *
 * The buffer is armed only while a single dialog is frontmost and clears
 * when that dialog loses foreground: a capture armed across a whole
 * settings page is a page on which no key does anything (ADR 0021).
 */

/** The four modifiers a Mosaix combination can carry. */
export type Modifier = "ctrl" | "alt" | "shift" | "win";

/** The modifier flags a keyboard or mouse event reports. */
export interface ModifierState {
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/** A key event as the capture buffer reads it. */
export interface CaptureKeyEvent extends ModifierState {
  /** The layout-independent physical key, e.g. `KeyJ` or `ArrowLeft`. */
  code: string;
}

export interface CaptureSession {
  /** Whether a keypress currently reaches the buffer. */
  armed: boolean;
  /**
   * Modifiers that were already down when the dialog opened, and have not
   * been released since.
   *
   * They are ignored while they last. Opening the dialog with Enter while
   * Ctrl is held would otherwise leave Ctrl in every combination the user
   * then presses, including after they let go of it -- the stuck modifier
   * ADR 0021's snapshot exists to prevent.
   */
  ignoredModifiers: Modifier[];
  /** The combination captured so far, `undefined` until a key arrives. */
  captured: CapturedCombo | undefined;
}

export interface CapturedCombo {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  win: boolean;
  /**
   * The Mosaix key name, or `null` for a physical key Mosaix has no name
   * for -- which is a combination that cannot be saved, said rather than
   * silently dropped.
   */
  key: string | null;
}

const NAMED_KEYS: Record<string, string> = {
  ArrowLeft: "LEFT",
  ArrowRight: "RIGHT",
  ArrowUp: "UP",
  ArrowDown: "DOWN",
  Space: "SPACE",
  Tab: "TAB",
  Enter: "ENTER",
  NumpadEnter: "ENTER",
  Escape: "ESCAPE",
  Home: "HOME",
  End: "END",
  PageUp: "PAGEUP",
  PageDown: "PAGEDOWN",
  Insert: "INSERT",
  Delete: "DELETE",
  Backspace: "BACKSPACE",
};

const MODIFIER_CODES: Record<string, Modifier> = {
  ControlLeft: "ctrl",
  ControlRight: "ctrl",
  AltLeft: "alt",
  AltRight: "alt",
  ShiftLeft: "shift",
  ShiftRight: "shift",
  MetaLeft: "win",
  MetaRight: "win",
};

/**
 * The Mosaix key name for a physical key, or `null` when Mosaix has none.
 *
 * Read off `code` rather than `key`, so the combination is the physical
 * key the user pressed rather than the character their layout produces --
 * which is what `RegisterHotKey` binds.
 */
export function keyName(code: string): string | null {
  const named = NAMED_KEYS[code];
  if (named !== undefined) return named;
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  return null;
}

/** Whether `code` is a modifier key rather than one a combination ends on. */
export function isModifierCode(code: string): boolean {
  return code in MODIFIER_CODES;
}

/**
 * Opens the buffer, snapshotting whichever modifiers `held` reports as
 * already down.
 */
export function openCapture(held: ModifierState): CaptureSession {
  const ignored: Modifier[] = [];
  if (held.ctrlKey) ignored.push("ctrl");
  if (held.altKey) ignored.push("alt");
  if (held.shiftKey) ignored.push("shift");
  if (held.metaKey) ignored.push("win");
  return { armed: true, ignoredModifiers: ignored, captured: undefined };
}

/**
 * Records a key press.
 *
 * A modifier press is not itself a combination, so it leaves the buffer
 * alone: the combination is settled by the first key that is not a
 * modifier, with whichever modifiers are held at that moment.
 */
export function pressKey(
  session: CaptureSession,
  event: CaptureKeyEvent,
): CaptureSession {
  if (!session.armed) return session;
  if (isModifierCode(event.code)) return session;
  const active = (modifier: Modifier, held: boolean): boolean =>
    held && !session.ignoredModifiers.includes(modifier);
  return {
    ...session,
    captured: {
      ctrl: active("ctrl", event.ctrlKey),
      alt: active("alt", event.altKey),
      shift: active("shift", event.shiftKey),
      win: active("win", event.metaKey),
      key: keyName(event.code),
    },
  };
}

/**
 * Records a key release, which is how a modifier held from before the
 * dialog opened stops being ignored.
 */
export function releaseKey(
  session: CaptureSession,
  event: CaptureKeyEvent,
): CaptureSession {
  const modifier = MODIFIER_CODES[event.code];
  if (modifier === undefined) return session;
  return {
    ...session,
    ignoredModifiers: session.ignoredModifiers.filter(
      (ignored) => ignored !== modifier,
    ),
  };
}

/**
 * Disarms the buffer and throws away what it held, for a dialog that is
 * no longer frontmost.
 */
export function blurCapture(session: CaptureSession): CaptureSession {
  return { ...session, armed: false, captured: undefined };
}

/** Re-arms a dialog that is frontmost again, still holding nothing. */
export function focusCapture(session: CaptureSession): CaptureSession {
  return { ...session, armed: true };
}

/**
 * The combination in the spelling configuration files use, or `null` when
 * the buffer does not hold a complete one.
 *
 * Incomplete means one of three things: nothing pressed yet, a physical
 * key Mosaix has no name for, or a key with no modifier at all -- which
 * would bind a bare letter globally and swallow it from every other
 * application.
 */
export function capturedCombo(session: CaptureSession): string | null {
  const captured = session.captured;
  if (captured === undefined || captured.key === null) return null;
  const parts: string[] = [];
  if (captured.ctrl) parts.push("ctrl");
  if (captured.alt) parts.push("alt");
  if (captured.shift) parts.push("shift");
  if (captured.win) parts.push("win");
  if (parts.length === 0) return null;
  parts.push(captured.key.toLowerCase());
  return parts.join("+");
}

/**
 * What the dialog shows while the user is still pressing, including the
 * incomplete states `capturedCombo` refuses to return.
 */
export function capturedLabel(session: CaptureSession): string {
  const captured = session.captured;
  if (captured === undefined) return "Press a combination…";
  if (captured.key === null) return "Mosaix has no name for that key";
  const complete = capturedCombo(session);
  if (complete === null) return "Add a modifier — Ctrl, Alt, Shift, or Win";
  return complete;
}
