// Copying a secret shown once (Plan.md P8.7, P8.8): the recovery codes and an invite's
// code. The clipboard, and Windows' clipboard history, may keep a copy, which the toast
// says; when copying fails (no clipboard, a refusal) the text is still on screen to copy
// by hand.
import { toast } from "sonner";

/** Copies `text`, then toasts `copied`, or `failed` when that didn't work. */
export async function copyWithToast(text: string, copied: string, failed: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    toast.success(copied);
  } catch {
    toast.error(failed);
  }
}
