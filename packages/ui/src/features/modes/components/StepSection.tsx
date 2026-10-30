// One section of a mode (Plan.md P11.9; ADR-0020, U68, R158, R167): its heading and hint, its steps
// in their order, and its own Add step. A step moves only within its section: each section keeps
// its own list and its own drag context, so nothing can carry a step to another one. Dragging
// goes by pointer events, since Tauri's file drop stops HTML5 drag events on Windows; the handle
// reorders by keyboard too, and Move up and Move down do without dragging (WCAG 2.2). The server
// judges every rule but the most steps a form offers.
import {
  closestCenter,
  DndContext,
  type DragEndEvent,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import {
  SortableContext,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { GripVerticalIcon } from "lucide-react";
import { useId } from "react";
import { type UseFormReturn, useFieldArray } from "react-hook-form";

import { Button } from "../../../components/ui/button";
import { dragAnnouncements } from "../dragging";
import { type ModeValues, newStep, type Section } from "../modeForm";
import {
  ADD_STEP,
  AT_MOST_STEPS,
  DRAG_INSTRUCTIONS,
  dragStep,
  SECTION_TEXT,
  stepMoved,
} from "../texts";
import { UnknownSteps } from "./ModeParts";
import { StepCard, type StepView } from "./StepCard";

/** How far a pointer moves on the handle before it's a drag, not a click. */
const DRAG_FROM_PX = 4;

/** What a step in a section needs from it. */
interface StepPlace {
  readonly form: UseFormReturn<ModeValues>;
  readonly section: Section;
  readonly count: number;
  readonly view: StepView;
  readonly onMove: (from: number, to: number) => void;
  readonly onRemove: (index: number) => void;
}

/** A step's card with its drag handle. */
function DraggableStep({ id, index, place }: { id: string; index: number; place: StepPlace }) {
  const {
    attributes,
    isDragging,
    listeners,
    setActivatorNodeRef,
    setNodeRef,
    transform,
    transition,
  } = useSortable({ id, disabled: place.view.locked });
  return (
    <div
      ref={setNodeRef}
      className={isDragging ? "relative z-10 bg-background" : undefined}
      style={{ transform: CSS.Transform.toString(transform), transition }}
    >
      <StepCard
        form={place.form}
        section={place.section}
        index={index}
        count={place.count}
        view={place.view}
        onMove={place.onMove}
        onRemove={place.onRemove}
        handle={
          <button
            ref={setActivatorNodeRef}
            type="button"
            className="cursor-grab touch-none self-start rounded-sm text-muted-foreground disabled:cursor-default"
            {...attributes}
            {...listeners}
            aria-label={dragStep(index + 1)}
            disabled={place.view.locked}
          >
            <GripVerticalIcon aria-hidden="true" className="size-4" />
          </button>
        }
      />
    </div>
  );
}

/** `section` of `form`; `unknown` steps show as a line each, `full` when no more can be added. */
export function StepSection({
  form,
  section,
  view,
  full,
  unknown,
  announce,
}: {
  form: UseFormReturn<ModeValues>;
  section: Section;
  view: StepView;
  full: boolean;
  unknown: number;
  announce: (text: string) => void;
}) {
  const hintId = useId();
  const steps = useFieldArray({ control: form.control, name: section });
  const text = SECTION_TEXT[section];
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: DRAG_FROM_PX } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );
  const ids = steps.fields.map((field) => field.id);

  function move(from: number, to: number): void {
    steps.move(from, to);
    announce(stepMoved(from + 1, to + 1, steps.fields.length));
  }

  /** A drop: the step takes the place of the one it's over; the context's own region says so. */
  function dropped({ active, over }: DragEndEvent): void {
    if (over !== null && active.id !== over.id) {
      steps.move(ids.indexOf(String(active.id)), ids.indexOf(String(over.id)));
    }
  }

  const place: StepPlace = {
    form,
    section,
    count: steps.fields.length,
    view,
    onMove: move,
    onRemove: steps.remove,
  };
  return (
    <fieldset className="flex flex-col gap-4" aria-describedby={hintId}>
      <legend className="mb-2 text-lg font-semibold">{text.heading}</legend>
      <p id={hintId} className="text-sm text-muted-foreground">
        {text.hint}
      </p>
      {view.readOnly ? (
        steps.fields.map((field, index) => (
          <StepCard
            key={field.id}
            form={form}
            section={section}
            index={index}
            count={steps.fields.length}
            view={view}
            onMove={move}
            onRemove={steps.remove}
          />
        ))
      ) : (
        <DndContext
          sensors={sensors}
          collisionDetection={closestCenter}
          accessibility={{
            announcements: dragAnnouncements(ids),
            screenReaderInstructions: { draggable: DRAG_INSTRUCTIONS },
          }}
          onDragEnd={dropped}
        >
          <SortableContext items={ids} strategy={verticalListSortingStrategy}>
            {steps.fields.map((field, index) => (
              <DraggableStep key={field.id} id={field.id} index={index} place={place} />
            ))}
          </SortableContext>
        </DndContext>
      )}
      <UnknownSteps count={unknown} />
      {view.readOnly ? null : (
        <div className="flex flex-wrap items-center gap-3">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={view.locked || full}
            onClick={() => steps.append(newStep())}
          >
            {ADD_STEP}
          </Button>
          {full ? <span className="text-sm text-muted-foreground">{AT_MOST_STEPS}</span> : null}
        </div>
      )}
    </fieldset>
  );
}
