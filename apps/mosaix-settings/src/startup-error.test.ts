import { describe, expect, it } from "vitest";

import { renderStartupError } from "./startup-error";

describe("startup error", () => {
  it("renders backend failures as text rather than privileged WebView markup", () => {
    const root = document.createElement("div");

    renderStartupError(root, '<img src=x onerror="alert(1)">');

    expect(root.querySelector("img")).toBeNull();
    expect(root.querySelector("[data-error-message]")?.textContent).toBe(
      '<img src=x onerror="alert(1)">',
    );
  });
});
