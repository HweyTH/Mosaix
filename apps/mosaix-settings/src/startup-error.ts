export function renderStartupError(root: HTMLElement, error: unknown): void {
  root.innerHTML = `
    <main class="startup-error">
      <div class="brand-mark" aria-hidden="true"><i></i><i></i><i></i><i></i></div>
      <h1>Mosaix could not start</h1>
      <p data-error-message></p>
    </main>`;
  root.querySelector<HTMLElement>("[data-error-message]")!.textContent = String(error);
}
