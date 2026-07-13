import { Skeleton } from "@/components/ui/skeleton";

/** Shimmer placeholder rows matching list layouts (DESIGN.md §5). */
export function SkeletonRows({ count = 5 }: { count?: number }) {
  return (
    <div className="flex flex-col gap-1.5 py-1" aria-label="Loading">
      {Array.from({ length: count }).map((_, index) => (
        <Skeleton className="h-16" key={index} />
      ))}
    </div>
  );
}
