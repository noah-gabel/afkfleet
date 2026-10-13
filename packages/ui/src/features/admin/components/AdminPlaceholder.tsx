// The admin screens until group E builds them (Plan.md P8.8): their routes and role guard
// exist, so the navigation is role-aware from P8.6 on.

const SCREENS = {
  users: { title: "Users", text: "User management arrives in the next update." },
  invites: { title: "Invites", text: "Invite management arrives in the next update." },
} as const;

/** The placeholder of the users or the invites screen. */
export function AdminPlaceholder({ what }: { what: keyof typeof SCREENS }) {
  const screen = SCREENS[what];
  return (
    <main className="flex flex-col gap-2 p-6">
      <h1 className="text-2xl font-semibold tracking-tight">{screen.title}</h1>
      <p className="text-muted-foreground">{screen.text}</p>
    </main>
  );
}
