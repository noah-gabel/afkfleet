// zod for the whole UI (ADR-0017): every module imports `z` from here, never from "zod"
// (Biome refuses it), so this runs before any schema exists. The release CSP has no
// 'unsafe-eval', and zod 4 probes for a JIT with `new Function("")` at its first object
// parse; the probe is caught, but WebView2 still reports a CSP violation at every start.
// `jitless` skips the probe and the compiled parsers; zod then parses the plain way.
import { z } from "zod";

z.config({ jitless: true });

export { z };
