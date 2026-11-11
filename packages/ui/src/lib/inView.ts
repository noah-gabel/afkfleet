// Brings what just appeared to where the user is (ADR-0020, L4, PR #92's review): a scroller can
// hold it out of view, as a stacked editor holds its notes below its steps, or a server's formats
// hold Try a line's answer below them. A ref callback, one function for every element, so React
// calls it only as an element mounts.

/** Scrolls a new notice into its scrollers' view, as little as it takes. */
export function inView(element: HTMLElement | null): void {
  element?.scrollIntoView({ block: "nearest", inline: "nearest" });
}
