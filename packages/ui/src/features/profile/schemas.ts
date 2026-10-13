// The profile's form schemas (Plan.md P8.7), built on the API's own (api/schemas.ts), so a
// form says what's wrong before anything is sent; the server checks again, and its
// strength check is the server's alone.
import { newPassword } from "../../api/schemas";
import { z } from "../../api/zod";

/** The new password, typed twice; the repeat is never sent. */
export const newPasswordForm = z
  .object({ password: newPassword, repeat: z.string() })
  .refine((values) => values.password === values.repeat, {
    path: ["repeat"],
    message: "The passwords don't match.",
  });

/** The new password form's values. */
export type NewPasswordValues = z.infer<typeof newPasswordForm>;
