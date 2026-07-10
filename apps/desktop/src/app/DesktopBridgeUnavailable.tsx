export function DesktopBridgeUnavailable() {
  return (
    <main className="grid min-h-screen place-items-center bg-background p-6 text-foreground">
      <section
        aria-labelledby="desktop-bridge-unavailable-title"
        className="w-full max-w-xl rounded-xl border border-border bg-card p-8 shadow-dialog"
        role="alert"
      >
        <p className="mb-3 font-mono text-label font-semibold uppercase tracking-[0.06em] text-destructive">
          Startup blocked
        </p>
        <h1 id="desktop-bridge-unavailable-title" className="text-title-lg font-semibold">
          Desktop bridge unavailable
        </h1>
        <p className="mt-4 text-body-lg text-muted-foreground">
          Grok Desktop could not connect to its isolated Electron bridge. Close this window and reopen the desktop app.
        </p>
        <p className="mt-3 text-body text-subtle-foreground">
          Browser-only interface previews must be started with the explicit preview command.
        </p>
      </section>
    </main>
  );
}
