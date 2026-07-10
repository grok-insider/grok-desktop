import { clsx, type ClassValue } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";

/*
 * The DESIGN.md type ramp defines custom text-* font-size utilities. Without
 * registering them, tailwind-merge classifies e.g. "text-label" as a text
 * COLOR and drops it whenever a color like "text-info" follows (and vice
 * versa: "text-body-sm" in a size variant knocks out the variant's color).
 */
const twMerge = extendTailwindMerge({
  extend: {
    classGroups: {
      "font-size": ["text-label", "text-body-sm", "text-body", "text-body-lg", "text-title-sm", "text-title", "text-title-lg", "text-display"],
    },
  },
});

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
