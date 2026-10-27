import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

// A settings card's rows (shadcn's settings pages): the label with a muted line under it on the
// left, the control on the right, and a rule between rows. A compact control stays on the right
// on a phone; a `wide` one (a field, a segmented control) goes under its label. Shared by the
// profile and its placeholder (islands/ProfileSkeleton.tsx).

/** The rows of one card, with a rule between them. */
export function SettingList({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      className={cn('divide-y [&>*]:py-4 [&>:first-child]:pt-0 [&>:last-child]:pb-0', className)}
      {...props}
    />
  );
}

/** One row. `lead` goes before the label (a picture); `note` under the whole row (what an action
 * said, or the rest of an action that opened). */
export function Setting({
  title,
  htmlFor,
  description,
  descriptionId,
  lead,
  note,
  wide,
  children,
}: {
  title: ReactNode;
  htmlFor?: string;
  description?: ReactNode;
  descriptionId?: string;
  lead?: ReactNode;
  note?: ReactNode;
  wide?: boolean;
  children?: ReactNode;
}) {
  const Title = htmlFor ? 'label' : 'div';
  return (
    <div>
      <div className={cn('flex items-center justify-between gap-4 sm:gap-6', wide && 'max-sm:flex-col max-sm:items-stretch max-sm:gap-3')}>
        <div className="flex min-w-0 items-center gap-4">
          {lead}
          <div className="flex min-w-0 flex-col gap-0.5">
            <Title htmlFor={htmlFor} className="text-sm font-medium">
              {title}
            </Title>
            {description && (
              <div id={descriptionId} className="text-sm text-muted-foreground">
                {description}
              </div>
            )}
          </div>
        </div>
        {children && (
          <div data-wide={wide || undefined} className="flex shrink-0 items-center justify-end gap-2 max-sm:data-wide:w-full">
            {children}
          </div>
        )}
      </div>
      {note && <div className="mt-4 flex flex-col gap-3">{note}</div>}
    </div>
  );
}
