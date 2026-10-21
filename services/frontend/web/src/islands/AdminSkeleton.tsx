import { Search } from 'lucide-react';
import { Card, CardContent } from '@/components/ui/card';
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
export const USER_CELLS = ['h-5 w-24', 'h-5 w-40', 'h-6 w-44 rounded-4xl'];
export const ROLES_HEAD = ['Role', 'Permissions'];
export const ROLE_CELLS = ['h-5 w-24', 'h-6 w-72 rounded-4xl'];
export const INVITES_HEAD = ['Note', 'Role', 'Used', 'Until (UTC)', 'Status'];
export const INVITE_CELLS = ['h-5 w-28', 'h-5 w-20 rounded-4xl', 'h-5 w-10', 'h-5 w-20', 'h-5 w-16 rounded-4xl', ''];

/** The tabs, with `children` as the open tab's contents. */
export function AdminTabs({ children, ...props }: React.ComponentProps<typeof Tabs>) {
  return (
    <Tabs className="gap-4" {...props}>
      <TabsList>
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
    <div className="relative w-full sm:max-w-xs">
      <Search aria-hidden="true" className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
      <Input type="search" className="pl-9" {...props} />
    </div>
  );
}

/** A table in a card: the column heads on a muted band, the rows under them. With `actions`, a
 * last column of buttons, named for assistive technology only. */
export function TableCard({ head, actions = false, children }: { head: string[]; actions?: boolean; children: React.ReactNode }) {
  return (
    <Card className="gap-0 py-0">
      <Table className="[&_td:first-child]:pl-5 [&_td:last-child]:pr-5 [&_th:first-child]:pl-5 [&_th:last-child]:pr-5">
        <TableHeader className="bg-muted/50">
          <TableRow className="hover:bg-transparent">
            {head.map((h) => (
              <TableHead key={h}>{h}</TableHead>
            ))}
            {actions && (
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>{children}</TableBody>
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
export function PlaceholderRows({ cells, rows, fill = false }: { cells: string[]; rows: number; fill?: boolean }) {
  return Array.from({ length: rows }, (_, row) => (
    <TableRow key={row} aria-hidden="true" className="hover:bg-transparent">
      {cells.map((shape, cell) => (
        <TableCell key={cell} className={cn(fill && cell === cells.length - 1 && 'w-full min-w-56')}>
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

/** A listing's place while the panel loads: a full page of rows, with a pulse where each row's
 * details go. */
export function ListingSkeleton({ head, cells }: { head: string[]; cells: string[] }) {
  return (
    <div className="flex flex-col gap-4">
      <Skeleton className="h-9 w-full rounded-4xl sm:max-w-xs" />
      <TableCard head={head}>
        <PlaceholderRows cells={cells} rows={PAGE_SIZE} fill />
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
      <AdminTabs defaultValue="users">
        <TabsContent value="users">
          <ListingSkeleton head={USERS_HEAD} cells={USER_CELLS} />
        </TabsContent>
      </AdminTabs>
    </div>
  );
}
