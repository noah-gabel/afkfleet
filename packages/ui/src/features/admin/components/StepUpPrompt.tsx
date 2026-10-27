// "Confirm it's you" inline (Plan.md P8.8): where an admin write waits for the password the
// server wants first (useStepUpAction). No dialog: Radix's scroll lock injects a `<style>`,
// which the release CSP blocks (ADR-0017).
import { StepUpForm } from "../../../components/StepUpForm";
import { Alert, AlertDescription, AlertTitle } from "../../../components/ui/alert";

/** What the prompt asks by default: a change's password. */
const MAKE_THIS_CHANGE = "Enter your password to make this change. It counts for 5 minutes.";

/**
 * The password prompt of the write `id` names, asking `text` (a change's, by default).
 * `onConfirm` throws what to show.
 */
export function StepUpPrompt({
  id,
  pending,
  onConfirm,
  onCancel,
  text = MAKE_THIS_CHANGE,
}: {
  id: string;
  pending: boolean;
  onConfirm: (password: string) => Promise<void>;
  onCancel: () => void;
  text?: string;
}) {
  return (
    <Alert>
      <AlertTitle>Confirm it's you</AlertTitle>
      <AlertDescription className="flex flex-col gap-3 text-foreground">
        <p>{text}</p>
        <StepUpForm id={id} pending={pending} onConfirm={onConfirm} onCancel={onCancel} />
      </AlertDescription>
    </Alert>
  );
}
