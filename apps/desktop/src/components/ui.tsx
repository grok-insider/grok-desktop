/*
 * Bridge over the shadcn primitives in ./ui/ — keeps the pre-migration public
 * API stable so views migrate view-by-view (see apps/desktop/DESIGN.md §10).
 * New code should import from "@/components/ui/*" directly.
 */
import type { ButtonHTMLAttributes, ReactNode } from "react";
import { LoaderCircle } from "lucide-react";
import type { RunState } from "../services/desktopClient";
import { Badge, type badgeVariants } from "@/components/ui/badge";
import { Button as ButtonPrimitive } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type { VariantProps } from "class-variance-authority";

export function IconButton({
  label,
  children,
  className = "",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string; children: ReactNode }) {
  return (
    <button
      data-slot="icon-button"
      className={cn(
        "inline-grid size-[34px] shrink-0 place-items-center rounded-md border border-transparent text-muted-foreground outline-none transition-[background-color,border-color,color] duration-150",
        "hover:bg-foreground/5 hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring",
        className,
      )}
      aria-label={label}
      title={label}
      {...props}
    >
      {children}
    </button>
  );
}

const buttonVariantMap = {
  primary: "default",
  secondary: "outline",
  ghost: "ghost",
  danger: "destructive",
} as const;

export function Button({
  children,
  variant = "secondary",
  loading = false,
  className = "",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  children: ReactNode;
  variant?: "primary" | "secondary" | "ghost" | "danger";
  loading?: boolean;
}) {
  return (
    <ButtonPrimitive variant={buttonVariantMap[variant]} className={className} {...props} disabled={loading || props.disabled}>
      {loading && <LoaderCircle size={15} className="animate-spin" aria-hidden="true" />}
      {children}
    </ButtonPrimitive>
  );
}

const stateLabels: Record<RunState, string> = {
  queued: "Queued",
  planning: "Planning",
  awaiting_approval: "Needs approval",
  running: "Running",
  paused: "Paused",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
  interrupted_needs_review: "Needs review",
};

const stateVariants: Record<RunState, VariantProps<typeof badgeVariants>["variant"]> = {
  queued: "neutral",
  planning: "info",
  awaiting_approval: "warning",
  running: "info",
  paused: "neutral",
  completed: "success",
  failed: "destructive",
  cancelled: "neutral",
  interrupted_needs_review: "warning",
};

export function RunStatus({ state }: { state: RunState }) {
  return (
    <Badge variant={stateVariants[state]}>
      <span className="size-[5px] rounded-full bg-current" aria-hidden="true" />
      {stateLabels[state]}
    </Badge>
  );
}

export function Toggle({
  checked,
  onChange,
  label,
  disabled = false,
}: {
  checked: boolean;
  onChange(checked: boolean): void;
  label: string;
  disabled?: boolean;
}) {
  return <Switch checked={checked} onCheckedChange={onChange} aria-label={label} disabled={disabled} />;
}

export function PageHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description: string;
  actions?: ReactNode;
}) {
  return (
    <header className="mx-auto mb-7 flex max-w-[1440px] items-start justify-between gap-6 max-[680px]:flex-col max-[680px]:gap-3">
      <div>
        <h1 className="m-0 mb-1 text-title-lg font-semibold text-foreground">{title}</h1>
        <p className="m-0 text-body text-muted-foreground">{description}</p>
      </div>
      {actions && <div className="flex items-center gap-2 max-[680px]:w-full max-[680px]:*:flex-1">{actions}</div>}
    </header>
  );
}

export function SkeletonRows({ count = 5 }: { count?: number }) {
  return (
    <div className="flex flex-col gap-1.5 py-1" aria-label="Loading">
      {Array.from({ length: count }).map((_, index) => (
        <Skeleton className="h-16" key={index} />
      ))}
    </div>
  );
}
