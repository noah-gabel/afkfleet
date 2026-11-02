// afkfleet's name and logo (Plan.md P11.11; ADR-0020, U94, U95, S119). The wordmark is one word in
// two weights, so a screen reader and a search get "afkfleet" while the eye gets the two spans. The
// logo is a bundled image: Orbit's tile keeps its own colours in both themes, as an image can't take
// a theme's tokens, and that's the one place the app's colours aren't tokens. It's decorative beside
// the wordmark, which always holds the name.
import logo from "../assets/orbit.svg";

/** `afkfleet`: afk regular, fleet bold, in Geist. */
export function Wordmark() {
  return (
    <>
      <span className="sr-only">afkfleet</span>
      <span aria-hidden="true" data-slot="wordmark">
        afk<span className="font-bold">fleet</span>
      </span>
    </>
  );
}

/** Orbit, `size` CSS pixels square. */
export function Logo({ size = 24 }: { size?: number }) {
  return <img src={logo} alt="" width={size} height={size} className="shrink-0" />;
}
