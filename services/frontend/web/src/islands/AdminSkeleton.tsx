import { Search } from 'lucide-react';
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import { Field, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { PAGE_SIZE } from '@/lib/admin';
import { cn } from '@/lib/utils';

// The admin panel's frame, shared by the panel (islands/AdminPanel.tsx) and its placeholder, so
// the two can't drift apart in size: the tabs, a listing's search box, its table in a card and
// the line under it. Free of the API client: the placeholder is rendered when the page is built.
// The placeholder is the usual answer: a full first page of users, with more pages after it
// (e2e/admin.spec.ts measures it).

export const TABS = [
  ['users', 'Users'],
  ['roles', 'Roles'],
  ['invites', 'Invites'],
] as const;

/** Each table's column heads, and the shape of its row's cells while it loads (PlaceholderRows):
 * a user's name, address and roles' badges; a role's name and permissions' badges; an invite's
 * note, role badge, uses, date and status badge, and no pulse for its button. */
export const USERS_HEAD = ['Username', 'Email', 'Roles'];
export const USERS_COLS = ['lg:w-[22%]', 'lg:w-[32%]', ''];
export const USER_CELLS = ['h-5 w-24', 'h-5 w-40', 'h-6 w-44 rounded-4xl'];
export const ROLES_HEAD = ['Role', 'Permissions'];
export const ROLES_COLS = ['lg:w-[22%]', ''];
export const ROLE_CELLS = ['h-5 w-24', 'h-6 w-72 rounded-4xl'];
/** The usual answer: a handful of roles, far fewer than a page. */
export const ROLE_ROWS = 5;
export const INVITES_HEAD = ['Note', 'Role', 'Used', 'Until (UTC)', 'Status'];
export const INVITES_COLS = ['', 'lg:w-[12%]', 'lg:w-[9%]', 'lg:w-[13%]', 'lg:w-[12%]', 'lg:w-[12%]'];
export const INVITE_CELLS = ['h-5 w-28', 'h-5 w-20 rounded-4xl', 'h-5 w-10', 'h-5 w-20', 'h-5 w-16 rounded-4xl', ''];

/** Below `lg` the table sizes its columns to what is in them; from `lg` the widths are fixed (the
 * `*_COLS`, as shares of the card; the column without one takes the rest), so the columns stay put
 * from page to page and search to search, and a long address is cut short (its whole text on
 * hover) instead of pushing the roles aside. */
export const CUT = 'lg:truncate';

/** On a phone a row is no table row but a stack of its cells (the heads are left to assistive
 * technology): one cell under the other for a user and a role; for an invite, the note on a
 * line of its own and the rest wrapping after it. */
export const STACKED = 'max-sm:grid max-sm:gap-1 max-sm:px-5 max-sm:py-3';
export const STACKED_WRAP =
  'max-sm:flex max-sm:flex-wrap max-sm:items-center max-sm:gap-x-3 max-sm:gap-y-1 max-sm:px-5 max-sm:py-3 max-sm:[&>td:first-child]:basis-full';
/** The cell with the badges takes the room left. */
export const FILL_CELL = 'w-full min-w-56 whitespace-normal max-sm:min-w-0 lg:min-w-0';

/** The tabs, with `children` as the open tab's contents. */
export function AdminTabs({ children, inert, ...props }: React.ComponentProps<typeof Tabs> & { inert?: boolean }) {
  return (
    <Tabs className="gap-4" {...props}>
      {/* Inert in the placeholder: before the page's scripts run, a tab can't open. */}
      <TabsList inert={inert}>
        {TABS.map(([value, label]) => (
          <TabsTrigger key={value} value={value}>
            {label}
          </TabsTrigger>
        ))}
      </TabsList>
      {children}
    </Tabs>
  );
}

/** A listing's search box, with the magnifier inside it. */
export function SearchField(props: React.ComponentProps<typeof Input>) {
  return (
    <div className="relative w-full sm:max-w-sm">
      <Search aria-hidden="true" className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
      <Input type="search" className="pl-9" {...props} />
    </div>
  );
}

/** A table in a card: the column heads on a muted band, the rows under them. With `actions`, a
 * last column of buttons, named for assistive technology only. */
export function TableCard({
  head,
  cols,
  actions = false,
  children,
}: {
  head: string[];
  cols: string[];
  actions?: boolean;
  children: React.ReactNode;
}) {
  return (
    <Card className="gap-0 py-0">
      {/* Explicit roles: a phone's stacked rows aren't table layout, and some browsers then drop the table. */}
      <Table
        role="table"
        className={cn(
          'lg:table-fixed sm:[&_td:first-child]:pl-5 sm:[&_td:last-child]:pr-5 [&_th:first-child]:pl-5 [&_th:last-child]:pr-5',
          'max-sm:block max-sm:[&_tbody]:block max-sm:[&_td]:p-0',
        )}
      >
        <TableHeader role="rowgroup" className="bg-muted/50 max-sm:sr-only">
          <TableRow role="row" className="hover:bg-transparent">
            {head.map((h, i) => (
              <TableHead key={h} role="columnheader" className={cols[i]}>
                {h}
              </TableHead>
            ))}
            {actions && (
              <TableHead role="columnheader" className={cols[head.length]}>
                <span className="sr-only">Actions</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody role="rowgroup">{children}</TableBody>
      </Table>
    </Card>
  );
}

/** An empty listing: a card with why, in place of the table (whose heads alone would be wider
 * than a phone, with nothing to scroll to). */
export function EmptyCard({ children }: { children: React.ReactNode }) {
  return (
    <Card>
      <CardContent className="text-center text-muted-foreground">{children}</CardContent>
    </Card>
  );
}

/** Rows of pulses while a table loads, one per cell, each the shape of what goes there (none for
 * an empty shape). With `fill`, the last column takes the room left, as a listing's badges do. */
export function PlaceholderRows({
  cells,
  rows,
  fill = false,
  stacked = STACKED,
}: {
  cells: string[];
  rows: number;
  fill?: boolean;
  stacked?: string;
}) {
  return Array.from({ length: rows }, (_, row) => (
    <TableRow key={row} aria-hidden="true" className={cn('hover:bg-transparent', stacked)}>
      {cells.map((shape, cell) => (
        <TableCell key={cell} className={cn(fill && cell === cells.length - 1 && FILL_CELL)}>
          {shape && <Skeleton className={cn('rounded-md', shape)} />}
        </TableCell>
      ))}
    </TableRow>
  ));
}

/** The line under a listing: how many there are, and the pager (a pager's height even without
 * one, so a listing that fits on one page is the same size). */
export function ListingFooter({ children }: { children: React.ReactNode }) {
  return <div className="flex min-h-8 flex-wrap items-center justify-between gap-3">{children}</div>;
}

/** The new-invite form's text, shared with its placeholder. */
export const INVITE_FORM = {
  title: 'New invite link',
  intro: 'Whoever opens it gets the role: logged out, they log in or sign up first.',
  role: 'Gives the role',
  note: "Note (who it's for)",
  days: 'Works for (days)',
  uses: 'How many people can use it (empty = no limit)',
  make: 'Make link',
};

/** The form's fields: two across, and from `lg` all four in a row. */
export const INVITE_FIELDS = 'grid gap-4 sm:grid-cols-2 lg:grid-cols-4 lg:items-end';

/** The invites tab's place until the roles (its form's choices) are in: the form card with its
 * own text and a pulse for each field and the button, then the list's. */
export function InvitesSkeleton() {
  return (
    <div className="flex flex-col gap-6">
      <Card>
        <CardHeader>
          <CardTitle>
            <h2>{INVITE_FORM.title}</h2>
          </CardTitle>
          <CardDescription>{INVITE_FORM.intro}</CardDescription>
        </CardHeader>
        <CardContent className={INVITE_FIELDS}>
          {[INVITE_FORM.role, INVITE_FORM.note, INVITE_FORM.days, INVITE_FORM.uses].map((label) => (
            <Field key={label}>
              <FieldLabel>{label}</FieldLabel>
              <Skeleton className="h-9 w-full rounded-4xl" />
            </Field>
          ))}
        </CardContent>
        <CardFooter>
          <span
            aria-hidden="true"
            className="inline-flex h-9 animate-pulse items-center rounded-4xl bg-muted px-3 text-sm font-medium text-transparent select-none"
          >
            {INVITE_FORM.make}
          </span>
        </CardFooter>
      </Card>
      <TableCard head={INVITES_HEAD} cols={INVITES_COLS} actions>
        <PlaceholderRows cells={INVITE_CELLS} rows={3} stacked={STACKED_WRAP} />
      </TableCard>
    </div>
  );
}

/** A listing's place while the panel loads: `rows` rows (the usual answer), with a pulse where each row's
 * details go. */
export function ListingSkeleton({ head, cols, cells, rows }: { head: string[]; cols: string[]; cells: string[]; rows: number }) {
  return (
    <div className="flex flex-col gap-4">
      <Skeleton className="h-9 w-full rounded-4xl sm:max-w-sm" />
      <TableCard head={head} cols={cols}>
        <PlaceholderRows cells={cells} rows={rows} fill />
      </TableCard>
      <ListingFooter>
        <Skeleton className="h-5 w-16 rounded-md" />
      </ListingFooter>
    </div>
  );
}

/** The panel's place until the page's scripts run: its users tab, loading. */
export function AdminSkeleton() {
  return (
    <div role="status" aria-label="Loading" aria-busy="true">
      <AdminTabs defaultValue="users" inert>
        <TabsContent value="users">
          <ListingSkeleton head={USERS_HEAD} cols={USERS_COLS} cells={USER_CELLS} rows={PAGE_SIZE} />
        </TabsContent>
      </AdminTabs>
    </div>
  );
}
