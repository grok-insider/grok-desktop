import * as React from "react";
import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

/*
 * No disabled:pointer-events-none here: views explain unavailable actions via
 * `<Button disabled title="...">`, and the native title tooltip needs hover.
 * The disabled attribute already blocks activation.
 */
const buttonVariants = cva(
  "inline-flex shrink-0 items-center justify-center gap-1.5 rounded-md text-body font-semibold whitespace-nowrap transition-[background-color,border-color,color,transform] duration-150 ease-fluid outline-none focus-visible:ring-[3px] focus-visible:ring-ring active:scale-[.98] disabled:opacity-50 disabled:active:scale-100 [&_svg]:pointer-events-none [&_svg]:shrink-0",
  {
    variants: {
      variant: {
        default: "bg-primary text-primary-foreground hover:bg-primary-hover disabled:hover:bg-primary",
        outline: "border border-input bg-card text-foreground hover:bg-muted hover:border-input-hover disabled:hover:bg-card disabled:hover:border-input",
        secondary: "bg-secondary text-secondary-foreground hover:bg-accent disabled:hover:bg-secondary",
        ghost: "text-muted-foreground hover:bg-muted hover:text-foreground disabled:hover:bg-transparent disabled:hover:text-muted-foreground",
        destructive: "border border-destructive/25 bg-destructive-soft text-destructive hover:border-destructive/40 disabled:hover:border-destructive/25",
        "destructive-solid": "bg-destructive text-destructive-foreground hover:bg-destructive-hover disabled:hover:bg-destructive",
        link: "text-foreground underline-offset-4 hover:underline",
      },
      size: {
        default: "h-[34px] px-3",
        sm: "h-7 rounded-md px-2.5 text-body-sm",
        lg: "h-9 rounded-lg px-4",
        icon: "size-[34px] rounded-md",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  },
);

function Button({
  className,
  variant,
  size,
  asChild = false,
  ...props
}: React.ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    asChild?: boolean;
  }) {
  const Comp = asChild ? Slot : "button";
  return <Comp data-slot="button" className={cn(buttonVariants({ variant, size, className }))} {...props} />;
}

export { Button, buttonVariants };
