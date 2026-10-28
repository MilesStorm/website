import { useId, type ReactNode } from 'react';
import { Card, CardContent } from '@/components/ui/card';
import { cn } from '@/lib/utils';

// A settings card's rows (shadcn's settings pages): the label with a muted line under it on the
// left, the control on the right, and a rule between rows. A compact control stays on the right
// on a phone; a `wide` one (a field, a segmented control) goes under its label. The rows follow
// the width of their card (a container query), not the window's, so a narrow card beside its
// heading on a laptop stacks the same way a phone's does. Shared by the profile and its
// placeholder (islands/ProfileSkeleton.tsx).

/** The profile's sections, one under the other: at most 64rem wide and flush left with the page
 * title; from `lg` a hairline between them, each section's text beside its card. From 2560 (a
 * column of 64rem would leave half the screen empty) two columns: the first section (Account)
 * on the left, the rest on the right, parted by space instead of hairlines, both tops level; each section's text above
 * its card there (Section), so the cards get the column's width. */
export const SECTIONS =
  'flex max-w-5xl flex-col gap-8 lg:gap-0 lg:divide-y lg:*:py-8 lg:*:first:pt-0 lg:*:last:pb-0 4xl:block 4xl:max-w-none 4xl:columns-2 4xl:gap-x-16 4xl:divide-y-0 4xl:*:break-inside-avoid 4xl:*:first:break-after-column 4xl:*:nth-2:pt-0';

/** The rows of one card, with a rule between them. */
function SettingList({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      className={cn('@container divide-y [&>*]:py-4 [&>:first-child]:pt-0 [&>:last-child]:pb-0', className)}
      {...props}
    />
  );
}

/** One section of a settings page (GitHub's, Vercel's): from `lg` its title and one line on the
 * left as plain text and its card of rows on the right (a third and two thirds); below, the text
 * above the card (and again from 2560, where the sections are in two columns: SECTIONS). `danger`
 * edges the card in a faint red, no louder than that. */
export function Section({
  title,
  description,
  danger,
  children,
}: {
  title: ReactNode;
  description?: ReactNode;
  danger?: boolean;
  children: ReactNode;
}) {
  const id = useId();
  return (
    <section aria-labelledby={id} className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,2fr)] lg:gap-x-8 4xl:grid-cols-1">
      <div className="flex flex-col gap-1 lg:pt-1 lg:pr-4 4xl:p-0">
        <h2 id={id} className="text-base font-medium">
          {title}
        </h2>
        {description && <p className="text-sm text-muted-foreground">{description}</p>}
      </div>
      <Card className={cn(danger && 'ring-destructive/40')}>
        <CardContent>
          <SettingList>{children}</SettingList>
        </CardContent>
      </Card>
    </section>
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
      <div className={cn('flex items-center justify-between gap-4 @lg:gap-6', wide && '@max-lg:flex-col @max-lg:items-stretch @max-lg:gap-3')}>
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
          <div data-wide={wide || undefined} className="flex min-w-0 shrink-0 items-center justify-end gap-2 @max-lg:data-wide:w-full @max-lg:data-wide:justify-start">
            {children}
          </div>
        )}
      </div>
      {note && <div className="mt-4 flex flex-col gap-3">{note}</div>}
    </div>
  );
}
