// A profile flow's first step (Plan.md P8.7): the password again ("Confirm it's you"),
// with the notice of a step that ended.
import { StepUpForm } from "../../../components/StepUpForm";
import { AuthCard } from "../../auth/components/AuthCard";

/** The password step titled `title`. `onConfirm` throws what the form should show. */
export function ConfirmStep({
  title,
  intro,
  notice,
  pending,
  onConfirm,
  onCancel,
}: {
  title: string;
  intro: string;
  notice: string | undefined;
  pending: boolean;
  onConfirm: (password: string) => Promise<void>;
  onCancel: () => void;
}) {
  return (
    <AuthCard title={title} intro={intro}>
      {notice === undefined ? null : (
        <p role="status" className="text-sm font-medium">
          {notice}
        </p>
      )}
      <StepUpForm
        id="confirm-password"
        pending={pending}
        onConfirm={onConfirm}
        onCancel={onCancel}
      />
    </AuthCard>
  );
}
