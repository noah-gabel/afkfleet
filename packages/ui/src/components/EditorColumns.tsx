// An editor's two columns (Plan.md P11.11; ADR-0020, U120, U121, R232), as a bot's screen has them
// (U105, U113): the thing's box on the left with Save and Cancel at its foot, then its notes, the
// password prompt and what's set apart (a copy, Delete); its long sections on the right. Each column
// scrolls inside itself, so Save stays in view while the sections scroll.
//
// Stacked, under 53rem of content, the parts follow the order they're in, which is the Tab order
// too: the box, the sections, the notes, Save and Cancel in a bar that sticks to the column's foot
// and never lies over the last section once that's scrolled to, then the rest. Two columns place
// the same parts by grid lines, so nothing moves in the document.
//
// The form wraps the box, the sections, the notes and the bar, but not the password prompt, which
// is a form of its own: it's `display: contents`, so its parts are the grid's. Whatever Tab
// scrolls to in the sections and the notes stops a bar's height above the column's foot, so the
// bar never covers the focus (WCAG 2.4.11).
import { cn } from "cn";
import type { FormEventHandler, ReactNode } from "react";

/** What an editor shows, by where it goes. */
export interface EditorParts {
  /** The editor's form, around everything but the prompt; a read-only view has none. */
  readonly form?: { readonly id: string; readonly onSubmit: FormEventHandler<HTMLFormElement> };
  /** The thing's own box: its heading, its facts and its fields. */
  readonly box: ReactNode;
  /** The long part: the sections, on the right. */
  readonly sections: ReactNode;
  /** What the form says about itself: its error, a conflict, a wait. */
  readonly notes?: ReactNode;
  /** Save and Cancel; a read-only view has none, and no bar. */
  readonly actions?: ReactNode;
  /** The password prompt, outside the form. */
  readonly prompt?: ReactNode;
  /** What's set apart at the foot: a copy, Delete. */
  readonly apart?: ReactNode;
}

/** The parts of an editor in their two columns, or stacked. */
export function EditorColumns({ form, box, sections, notes, actions, prompt, apart }: EditorParts) {
  const joined = actions !== undefined;
  const inForm = (
    <>
      <div
        data-slot="editor-box"
        className={cn(
          "flex min-w-0 flex-col gap-3 rounded-lg border bg-card p-3 @min-[53rem]/content:col-start-1 @min-[53rem]/content:row-start-1",
          joined
            ? "@min-[53rem]/content:rounded-b-none @min-[53rem]/content:border-b-0 @min-[53rem]/content:pb-0"
            : "@min-[53rem]/content:mb-3",
        )}
      >
        {box}
      </div>
      <div
        data-slot="editor-sections"
        className="relative flex min-w-0 flex-col gap-3 [&_*]:scroll-mb-16 @min-[53rem]/content:col-start-2 @min-[53rem]/content:row-span-5 @min-[53rem]/content:row-start-1 @min-[53rem]/content:-m-1 @min-[53rem]/content:min-h-0 @min-[53rem]/content:overflow-y-auto @min-[53rem]/content:scroll-p-1 @min-[53rem]/content:p-1"
      >
        {sections}
      </div>
      <div
        data-slot="editor-notes"
        className="flex min-w-0 flex-col gap-2 empty:hidden [&_*]:scroll-mb-16 @min-[53rem]/content:col-start-1 @min-[53rem]/content:row-start-3 @min-[53rem]/content:mb-3"
      >
        {notes}
      </div>
      {joined ? (
        <div
          data-slot="editor-actions"
          className="sticky bottom-0 z-10 flex flex-wrap gap-2 border-t bg-background py-2 @min-[53rem]/content:static @min-[53rem]/content:col-start-1 @min-[53rem]/content:row-start-2 @min-[53rem]/content:mb-3 @min-[53rem]/content:rounded-b-lg @min-[53rem]/content:border @min-[53rem]/content:border-t-0 @min-[53rem]/content:bg-card @min-[53rem]/content:px-3 @min-[53rem]/content:pt-3 @min-[53rem]/content:pb-3"
        >
          {actions}
        </div>
      ) : null}
    </>
  );
  return (
    <main className="flex flex-1 flex-col p-4 @min-[53rem]/content:min-h-0">
      <div className="grid gap-3 @min-[53rem]/content:min-h-0 @min-[53rem]/content:flex-[1_1_0] @min-[53rem]/content:grid-cols-[minmax(18rem,2fr)_3fr] @min-[53rem]/content:grid-rows-[auto_auto_auto_auto_minmax(0,1fr)] @min-[53rem]/content:gap-y-0">
        {form === undefined ? (
          inForm
        ) : (
          <form id={form.id} className="contents" noValidate onSubmit={form.onSubmit}>
            {inForm}
          </form>
        )}
        {prompt === undefined ? null : (
          <div
            data-slot="editor-prompt"
            className="flex min-w-0 flex-col gap-2 empty:hidden @min-[53rem]/content:col-start-1 @min-[53rem]/content:row-start-4 @min-[53rem]/content:mb-3"
          >
            {prompt}
          </div>
        )}
        {apart === undefined ? null : (
          <div
            data-slot="editor-apart"
            className="relative flex min-w-0 flex-col items-start gap-3 empty:hidden @min-[53rem]/content:col-start-1 @min-[53rem]/content:row-start-5 @min-[53rem]/content:-m-1 @min-[53rem]/content:min-h-0 @min-[53rem]/content:overflow-y-auto @min-[53rem]/content:p-1"
          >
            {apart}
          </div>
        )}
      </div>
    </main>
  );
}
