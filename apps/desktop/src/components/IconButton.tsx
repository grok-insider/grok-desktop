import type { ButtonHTMLAttributes, ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/**
 * 34px icon-only action built on the shadcn Button. `label` names the control
 * for assistive tech and doubles as the native tooltip.
 */
export function IconButton({
  label,
  children,
  className = "",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string; children: ReactNode }) {
  return (
    <Button
      data-slot="icon-button"
      variant="ghost"
      size="icon"
      className={cn("border border-transparent hover:bg-foreground/5", className)}
      aria-label={label}
      title={label}
      {...props}
    >
      {children}
    </Button>
  );
}
