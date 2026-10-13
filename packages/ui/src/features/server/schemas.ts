// The server form's zod schema (Plan.md P8.7, ADR-0017), built on the API's own
// (api/schemas.ts). A custom CA that's checked must be there: the UI never sees the
// current CA's PEM, so a save without it would silently fall back to the system's roots.

import { serverProfileRequest } from "../../api/schemas";
import { z } from "../../api/zod";

/** The hint when the current profile's CA must be given again. */
export const CA_AGAIN = "Paste or choose the CA again to keep it";
/** The hint when a newly checked custom CA is empty. */
export const CA_MISSING = "Paste or choose the CA's certificates.";

/** The form for a profile whose current CA (if any) `hasCurrentCa` says. */
export function serverForm(hasCurrentCa: boolean) {
  return z
    .object({
      url: z
        .string()
        .refine((text) => text.trim() !== "", "Enter the server's address.")
        .pipe(serverProfileRequest.shape.url),
      useCa: z.boolean(),
      caPem: serverProfileRequest.shape.ca_pem.unwrap(),
    })
    .superRefine((values, context) => {
      if (values.useCa && values.caPem.trim() === "") {
        context.addIssue({
          code: "custom",
          path: ["caPem"],
          message: hasCurrentCa ? CA_AGAIN : CA_MISSING,
        });
      }
    });
}

/** The server form's values. */
export type ServerValues = z.infer<ReturnType<typeof serverForm>>;
