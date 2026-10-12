/**
 * The app's root component. For now it only names the app; the API client (P8.5) and the
 * routes (P8.6) arrive with the next groups.
 */
export function App() {
  return (
    <main className="flex min-h-screen flex-col items-center justify-center gap-2 bg-background p-8 text-foreground">
      <h1 className="text-3xl font-semibold tracking-tight">afkfleet</h1>
      <p className="text-muted-foreground">The desktop app is being set up.</p>
    </main>
  );
}
