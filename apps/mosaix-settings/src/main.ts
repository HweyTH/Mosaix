// Self-hosted so the interface renders identically offline and on a
// machine that has never seen Inter. Weight axis only; the subsets carry
// unicode-range, so only Latin is fetched.
import "@fontsource-variable/inter/wght.css";

import "./styles.css";

import { createTauriDesktopBridge } from "./desktop-bridge";
import { mountLayoutEditor } from "./layout-editor";
import { renderStartupError } from "./startup-error";

const root = document.querySelector<HTMLElement>("#app");
if (!root) throw new Error("Mosaix settings root element is missing");

mountLayoutEditor(root, createTauriDesktopBridge()).catch((error: unknown) => {
  renderStartupError(root, error);
});
