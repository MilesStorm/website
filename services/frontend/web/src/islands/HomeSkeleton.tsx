import { Skeleton } from '@/components/ui/skeleton';

/** The logged-in start page's size while it loads, so nothing moves when it arrives. */
export function HomeSkeleton() {
  return (
    <div className="flex flex-col gap-10" aria-busy="true">
      <Skeleton className="h-[138px] rounded-xl" />
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        <Skeleton className="h-40 rounded-xl" />
        <Skeleton className="h-40 rounded-xl" />
      </div>
    </div>
  );
}
