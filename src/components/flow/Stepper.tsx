import { Check } from "lucide-react";
import { cn } from "@/lib/utils";

export interface StepperProps {
  /** 1-based index of the active step. */
  current: number;
  steps: string[];
}

/**
 * Compact horizontal step indicator for the Go Live → Record → Loop flow.
 * Purely presentational — the active step is driven by the parent.
 */
export function Stepper({ current, steps }: StepperProps) {
  return (
    <div className="flex items-center justify-center">
      {steps.map((label, i) => {
        const index = i + 1;
        const isActive = index === current;
        const isDone = index < current;
        return (
          <div key={label} className="flex items-center">
            <div className="flex items-center gap-2">
              <div
                className={cn(
                  "flex h-8 w-8 items-center justify-center rounded-full border text-sm font-semibold transition-colors",
                  isActive && "bg-primary text-primary-foreground border-primary shadow-glow",
                  isDone && "bg-primary/20 text-primary border-primary/40",
                  !isActive && !isDone && "bg-muted text-muted-foreground border-border"
                )}
              >
                {isDone ? <Check className="h-4 w-4" /> : index}
              </div>
              <span
                className={cn(
                  "text-sm font-medium hidden sm:inline",
                  isActive ? "text-foreground" : "text-muted-foreground"
                )}
              >
                {label}
              </span>
            </div>
            {index < steps.length && (
              <div className={cn("mx-3 h-px w-8 sm:w-12", isDone ? "bg-primary/40" : "bg-border")} />
            )}
          </div>
        );
      })}
    </div>
  );
}
