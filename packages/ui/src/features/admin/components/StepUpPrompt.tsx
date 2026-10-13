// "Confirm it's you" inline (Plan.md P8.8): where an admin write waits for the password the
// server wants first (useStepUpAction). No dialog: Radix's scroll lock injects a `<style>`,
// which the release CSP blocks (ADR-0017).
import { StepUpForm } from "../../../components/StepUpForm";
import { Alert, AlertDescription, AlertTitle } from "../../../components/ui/alert";

/** The password prompt of the write `id` names. `onConfirm` throws what to show. */
export function StepUpPrompt({
  id,
  pending,
  onConfirm,
  onCancel,
}: {
  id: string;
  pending: boolean;
  onConfirm: (password: string) => Promise<void>;
  onCancel: () => void;
}) {
  return (
    <Alert>
      <AlertTitle>Confirm it's you</AlertTitle>
      <AlertDescription className="flex flex-col gap-3 text-foreground">
        <p>Enter your password to make this change. It counts for 5 minutes.</p>
        <StepUpForm id={id} pending={pending} onConfirm={onConfirm} onCancel={onCancel} />
      </AlertDescription>
    </Alert>
  );
}
