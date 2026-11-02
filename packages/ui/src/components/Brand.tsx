// afkfleet's name and logo (Plan.md P11.11; ADR-0020, U94, U95, U104, S120). The wordmark is one word
// in two weights, so a screen reader and a search get "afkfleet" while the eye gets the two spans. The
// logo is Orbit's marks with no tile, drawn from the theme's tokens, so they read in both themes; the
// Windows icon keeps its tile. It's decorative beside the wordmark, which always holds the name.

/** Orbit's marks, as in apps/desktop/src-tauri/icons/icon.svg: Brand.test.tsx keeps them equal. */
const RING = { d: "M 700 262 A 312 312 0 1 1 262 324", strokeWidth: 88 };
const DOT = { cx: 512, cy: 512, r: 66 };
const SATELLITE = { cx: 768, cy: 226, r: 92 };

/**
 * The hole the satellite leaves in the ring: it overlaps the ring's end, and no two tokens part them
 * at 3:1. A clip keeps the ring's path whole and needs no colour, so it works on any background.
 */
const HOLE = SATELLITE.r + 32;
const GAP = `M 0 0 H 1024 V 1024 H 0 Z M ${SATELLITE.cx - HOLE} ${SATELLITE.cy} A ${HOLE} ${HOLE} 0 1 0 ${SATELLITE.cx + HOLE} ${SATELLITE.cy} A ${HOLE} ${HOLE} 0 1 0 ${SATELLITE.cx - HOLE} ${SATELLITE.cy} Z`;

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

/**
 * Orbit, `size` CSS pixels square, its view cropped to the marks now the tile's gone. A screen shows
 * one logo, so one clip's ID is enough.
 */
export function Logo({ size = 24 }: { size?: number }) {
  return (
    <svg
      data-slot="logo"
      viewBox="140 128 744 744"
      width={size}
      height={size}
      aria-hidden="true"
      className="shrink-0"
    >
      <clipPath id="orbit-gap">
        <path clipRule="evenodd" d={GAP} />
      </clipPath>
      <path
        {...RING}
        fill="none"
        strokeLinecap="round"
        clipPath="url(#orbit-gap)"
        className="stroke-primary"
      />
      <circle {...DOT} className="fill-primary" />
      <circle {...SATELLITE} className="fill-foreground" />
    </svg>
  );
}
