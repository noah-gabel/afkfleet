// The theme setting on the Profile screen (Plan.md P11.10; ADR-0020, U84): Light, Dark or Windows.
// It belongs to this PC, not the account, so the sign-in screens follow it too (S92).
import { useAppStores, useTheme } from "../../../app/context";
import type { ThemeChoice } from "../../../app/theme";

const CHOICES: readonly { choice: ThemeChoice; label: string; hint?: string }[] = [
  { choice: "light", label: "Light" },
  { choice: "dark", label: "Dark" },
  { choice: "windows", label: "Windows", hint: "Follows your Windows setting." },
];

/** The section. */
export function ThemeSection() {
  const chosen = useTheme((state) => state.choice);
  const store = useAppStores().theme;
  return (
    <section aria-labelledby="appearance-heading" className="flex flex-col gap-2">
      <h2 id="appearance-heading" className="text-lg font-semibold">
        Appearance
      </h2>
      <fieldset className="flex flex-col gap-2">
        <legend className="text-sm font-medium">Theme</legend>
        {CHOICES.map(({ choice, label, hint }) => (
          <div key={choice} className="flex flex-col">
            <label className="flex items-start gap-2 text-sm">
              <input
                type="radio"
                name="theme"
                value={choice}
                checked={chosen === choice}
                aria-describedby={hint === undefined ? undefined : `theme-hint-${choice}`}
                onChange={() => store.getState().choose(choice)}
                className="mt-1"
              />
              {label}
            </label>
            {hint === undefined ? null : (
              <span id={`theme-hint-${choice}`} className="ml-5 text-xs text-muted-foreground">
                {hint}
              </span>
            )}
          </div>
        ))}
      </fieldset>
    </section>
  );
}
