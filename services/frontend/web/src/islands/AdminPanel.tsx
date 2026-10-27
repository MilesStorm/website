import { useCallback, useEffect, useId, useState } from 'react';
import { ChevronLeft, ChevronRight, CircleCheck, Plus, X } from 'lucide-react';
import { Alert, AlertDescription } from '@/components/ui/alert';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Field, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { NativeSelect, NativeSelectOption } from '@/components/ui/native-select';
import { TableCell, TableRow } from '@/components/ui/table';
import { TabsContent } from '@/components/ui/tabs';
import { FormAlert } from '@/components/FormAlert';
import {
  AdminTabs,
  EmptyCard,
  FILL_CELL,
  INVITES_HEAD,
  INVITE_CELLS,
  INVITE_FORM,
  InvitesSkeleton,
  ListingFooter,
  ListingSkeleton,
  PlaceholderRows,
  ROLES_HEAD,
  ROLE_CELLS,
  ROLE_ROWS,
  SearchField,
  STACKED,
  STACKED_WRAP,
  TableCard,
  USERS_HEAD,
  USER_CELLS,
} from '@/islands/AdminSkeleton';
import { client, type Schemas } from '@/api/client';
import { readBootstrap } from '@/api/session';
import {
  DEFAULT_INVITE_ROLE,
  PAGE_SIZE,
  inviteStatus,
  inviteUses,
  newInvite,
  pageButtons,
  utcDate,
} from '@/lib/admin';
import { attempt, type Outcome } from '@/lib/errors';
import { isKnownPermission } from '@/lib/gates';
import { useMenu } from '@/lib/menu';
import { island } from '@/lib/island';

type RoleRef = Schemas['AdminRoleRef'];
type Permission = Schemas['AdminPermission'];

/** How long typing in a search box waits before asking. */
export const SEARCH_DELAY_MS = 250;

/** The admin panel: users and their roles, roles and their permissions, invite links. */
export const AdminPanel = island('AdminPanel', function AdminPanel() {
  // The small lists the pickers need: every role, every permission.
  const [roles, setRoles] = useState<Outcome<RoleRef[]> | null>(null);
  const [permissions, setPermissions] = useState<Outcome<Permission[]> | null>(null);
  // Who is looking: removing one of their own roles asks first.
  const [me, setMe] = useState<string | null>(null);
  useEffect(() => {
    void readBootstrap().then((b) => setMe(b.viewer?.username ?? null));
    void attempt(client.GET('/api/admin/roles/all')).then(setRoles);
    void attempt(client.GET('/api/admin/permissions')).then(setPermissions);
  }, []);

  // Until both are in, each tab is its placeholder (islands/AdminSkeleton.tsx); the tabs work
  // meanwhile.
  const loading = !roles || !permissions;
  const allRoles = roles?.ok ? roles.data : [];
  const allPermissions = permissions?.ok ? permissions.data : [];
  return (
    <div className="flex flex-col gap-4">
      {roles && !roles.ok && <FormAlert message={`Roles: ${roles.message}`} />}
      {permissions && !permissions.ok && <FormAlert message={`Permissions: ${permissions.message}`} />}
      <AdminTabs defaultValue="users">
        <TabsContent value="users">
          {loading ? (
            <ListingSkeleton head={USERS_HEAD} cells={USER_CELLS} rows={PAGE_SIZE} />
          ) : (
            <UsersTab allRoles={allRoles} me={me} />
          )}
        </TabsContent>
        <TabsContent value="roles">
          {loading ? (
            <ListingSkeleton head={ROLES_HEAD} cells={ROLE_CELLS} rows={ROLE_ROWS} />
          ) : (
            <RolesTab allPermissions={allPermissions} />
          )}
        </TabsContent>
        <TabsContent value="invites">
          {/* Needs only the roles, for its form's choices. */}
          {roles ? <InvitesTab allRoles={allRoles} /> : <InvitesSkeleton />}
        </TabsContent>
      </AdminTabs>
    </div>
  );
});

/** A searched, paged listing: what to show, and the controls' state. */
function useListing<T>(load: (page: number, search: string) => Promise<Outcome<{ items: T[]; total: number }>>) {
  const [search, setSearch] = useState('');
  const [asked, setAsked] = useState('');
  const [page, setPage] = useState(0);
  const [result, setResult] = useState<Outcome<{ items: T[]; total: number }> | null>(null);
  const [round, setRound] = useState(0);
  // A new search starts at the first page: both change together, so it is one request. Only a
  // changed term: the timer that runs on mount would otherwise undo a pager click.
  useEffect(() => {
    if (search === asked) return;
    const wait = setTimeout(() => {
      setAsked(search);
      setPage(0);
    }, SEARCH_DELAY_MS);
    return () => clearTimeout(wait);
  }, [search, asked]);
  useEffect(() => {
    let current = true;
    void load(page, asked).then((r) => current && setResult(r));
    return () => {
      current = false;
    };
  }, [load, page, asked, round]);
  return {
    search,
    setSearch,
    page,
    setPage,
    result,
    reload: useCallback(() => setRound((n) => n + 1), []),
  };
}

/** A user without an address: a dash in the table, words in the phone's stack (the heads are gone). */
const NO_EMAIL = (
  <>
    <span className="max-sm:hidden">—</span>
    <span className="sm:hidden">No email</span>
  </>
);

const loadUsers = (page: number, search: string) =>
  attempt(client.GET('/api/admin/users', { params: { query: { page, limit: PAGE_SIZE, search } } }));
const loadRoles = (page: number, search: string) =>
  attempt(client.GET('/api/admin/roles', { params: { query: { page, limit: PAGE_SIZE, search } } }));

function UsersTab({ allRoles, me }: { allRoles: RoleRef[]; me: string | null }) {
  const list = useListing<Schemas['AdminUser']>(loadUsers);
  const [error, setError] = useState<string | null>(null);
  const change = async (call: Promise<{ error?: unknown; response: Response }>) => {
    const done = await attempt(call);
    setError(done.ok ? null : done.message);
    list.reload();
  };
  return (
    <Listing
      label="Search users"
      placeholder="Search by username or email…"
      noun="user"
      list={list}
      error={error}
      head={USERS_HEAD}
      cells={USER_CELLS}
      rows={PAGE_SIZE}
      row={(user) => (
        <TableRow role="row" key={user.id} className={STACKED}>
          <TableCell role="cell" className="font-medium">{user.username}</TableCell>
          <TableCell role="cell" className="text-muted-foreground">{user.email ?? NO_EMAIL}</TableCell>
          <TableCell role="cell" className={FILL_CELL}>
            <Assigned
              owner={user.username}
              kind="role"
              have={user.roles.map((r) => ({ id: r.id, name: r.name }))}
              all={allRoles.map((r) => ({ id: r.id, name: r.name }))}
              warn={() => (user.username === me ? OWN_ROLE : null)}
              add={(roleId) =>
                void change(client.PUT('/api/admin/users/{user_id}/roles/{role_id}', { params: { path: { user_id: user.id, role_id: roleId } } }))
              }
              remove={(roleId) =>
                void change(
                  client.DELETE('/api/admin/users/{user_id}/roles/{role_id}', { params: { path: { user_id: user.id, role_id: roleId } } }),
                )
              }
            />
          </TableCell>
        </TableRow>
      )}
    />
  );
}

function RolesTab({ allPermissions }: { allPermissions: Permission[] }) {
  const list = useListing<Schemas['AdminRole']>(loadRoles);
  const [error, setError] = useState<string | null>(null);
  const change = async (call: Promise<{ error?: unknown; response: Response }>) => {
    const done = await attempt(call);
    setError(done.ok ? null : done.message);
    list.reload();
  };
  const named = (p: Permission) => ({ id: p.id, name: p.name });
  return (
    <Listing
      label="Search roles"
      placeholder="Search roles…"
      noun="role"
      list={list}
      error={error}
      head={ROLES_HEAD}
      cells={ROLE_CELLS}
      rows={ROLE_ROWS}
      row={(role) => (
        <TableRow role="row" key={role.id} className={STACKED}>
          <TableCell role="cell" className="font-medium">{role.name}</TableCell>
          <TableCell role="cell" className={FILL_CELL}>
            <Assigned
              owner={role.name}
              kind="permission"
              have={role.permissions.map(named)}
              all={allPermissions.map(named)}
              warn={(p) => (p.name === 'manage_permissions' ? MANAGE_PERMISSIONS : null)}
              add={(permissionId) =>
                void change(
                  client.PUT('/api/admin/roles/{role_id}/permissions/{permission_id}', {
                    params: { path: { role_id: role.id, permission_id: permissionId } },
                  }),
                )
              }
              remove={(permissionId) =>
                void change(
                  client.DELETE('/api/admin/roles/{role_id}/permissions/{permission_id}', {
                    params: { path: { role_id: role.id, permission_id: permissionId } },
                  }),
                )
              }
            />
          </TableCell>
        </TableRow>
      )}
    />
  );
}

/** A search box, the table, the count and the pager of a listing. `cells` and `rows`: the shape
 * of a row's cells while it loads (AdminSkeleton's PlaceholderRows), and how many. */
function Listing<T>({
  label,
  placeholder,
  noun,
  list,
  error,
  head,
  cells,
  rows,
  row,
}: {
  label: string;
  placeholder: string;
  noun: string;
  list: ReturnType<typeof useListing<T>>;
  error: string | null;
  head: string[];
  cells: string[];
  rows: number;
  row: (item: T) => React.ReactNode;
}) {
  const id = useId();
  const result = list.result;
  const total = result?.ok ? result.data.total : 0;
  return (
    <div className="flex flex-col gap-4">
      <label htmlFor={id} className="sr-only">
        {label}
      </label>
      <SearchField id={id} placeholder={placeholder} value={list.search} onChange={(e) => list.setSearch(e.target.value)} />
      <FormAlert message={error} />
      {result !== null && !result.ok ? (
        <FormAlert message={result.message} />
      ) : result?.data.items.length === 0 ? (
        <EmptyCard>No {noun}s found.</EmptyCard>
      ) : (
        <TableCard head={head}>
          {result === null ? <PlaceholderRows cells={cells} rows={rows} fill /> : result.data.items.map(row)}
        </TableCard>
      )}
      <ListingFooter>
        {result?.ok && (
          <p className="text-sm text-muted-foreground" aria-live="polite">
            {total} {noun}
            {total === 1 ? '' : 's'}
          </p>
        )}
        <Pager page={list.page} total={total} onPage={list.setPage} />
      </ListingFooter>
    </div>
  );
}

/** Why removing one of the viewer's own roles asks first. */
export const OWN_ROLE =
  "This is your own account. If this role is what gives you admin rights, you'll lose access to this page and can't give it back yourself.";
/** Why taking `manage_permissions` off a role asks first. */
export const MANAGE_PERMISSIONS =
  "manage_permissions is what lets anyone use this page. If your account has admin rights only through this role, you'll lose access to it and can't give it back yourself.";

/** What a user or role has (each removable), and a menu to add one more. A permission auth
 * has and this site doesn't know is marked, since nothing here checks it. A removal `warn` has
 * a reason for asks first. At least the add button's height, so a row without it is the same. */
function Assigned({
  owner,
  kind,
  have,
  all,
  warn,
  add,
  remove,
}: {
  owner: string;
  kind: 'role' | 'permission';
  have: { id: number; name: string }[];
  all: { id: number; name: string }[];
  warn: (item: { id: number; name: string }) => string | null;
  add: (id: number) => void;
  remove: (id: number) => void;
}) {
  const missing = all.filter((a) => !have.some((h) => h.id === a.id));
  const menu = useMenu();
  const unknown = (name: string) => kind === 'permission' && !isKnownPermission(name);
  return (
    <div className="flex min-h-6 flex-wrap items-center gap-1.5">
      {have.map((h) => (
        <Badge
          key={h.id}
          variant={unknown(h.name) ? 'outline' : 'secondary'}
          title={unknown(h.name) ? 'A permission this site doesn’t use yet' : undefined}
        >
          {h.name}
          {unknown(h.name) && <span> (unused)</span>}
          <RemoveButton
            action={`admin remove ${kind}`}
            label={`Remove ${kind} ${h.name} from ${owner}`}
            title={`Remove ${h.name} from ${owner}?`}
            warning={warn(h)}
            remove={() => remove(h.id)}
          />
        </Badge>
      ))}
      {missing.length > 0 && (
        <DropdownMenu {...menu.root}>
          <DropdownMenuTrigger asChild {...menu.trigger}>
            <Button size="xs" variant="outline" className="border-dashed" aria-label={`Add ${kind} to ${owner}`} data-faro-user-action-name={`admin add ${kind}`}>
              <Plus data-icon="inline-start" aria-hidden="true" />
              Add
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-auto">
            <DropdownMenuLabel>{kind === 'role' ? 'Give a role' : 'Grant a permission'}</DropdownMenuLabel>
            {missing.map((m) => (
              <DropdownMenuItem key={m.id} onSelect={() => add(m.id)}>
                {m.name}
                {unknown(m.name) && <span className="text-muted-foreground"> (unused)</span>}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      )}
    </div>
  );
}

/** A badge's remove button; with a `warning`, it asks first and says why. */
function RemoveButton({
  action,
  label,
  title,
  warning,
  remove,
}: {
  action: string;
  label: string;
  title: string;
  warning: string | null;
  remove: () => void;
}) {
  const button = (
    <button
      type="button"
      className="-mr-1 rounded-full p-0.5 text-muted-foreground outline-none hover:bg-foreground/10 hover:text-foreground focus-visible:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
      aria-label={label}
      data-faro-user-action-name={action}
      onClick={warning ? undefined : remove}
    >
      <X aria-hidden="true" className="size-3" />
    </button>
  );
  if (!warning) return button;
  return (
    <AlertDialog>
      <AlertDialogTrigger asChild>{button}</AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>{warning}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Keep</AlertDialogCancel>
          <AlertDialogAction variant="destructive" onClick={remove} data-faro-user-action-name={`${action} confirmed`}>
            Remove
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

function Pager({ page, total, onPage }: { page: number; total: number; onPage: (page: number) => void }) {
  const buttons = pageButtons(page, total);
  if (buttons.length === 0) return null;
  const last = Math.ceil(total / PAGE_SIZE) - 1;
  return (
    <nav aria-label="Pages" className="flex flex-wrap items-center justify-center gap-1">
      <Button size="sm" variant="ghost" disabled={page === 0} aria-label="Previous page" onClick={() => onPage(page - 1)} data-faro-user-action-name="admin page previous">
        <ChevronLeft aria-hidden="true" />
      </Button>
      {buttons.map((b, i) =>
        b === null ? (
          <span key={`gap-${i}`} className="px-1 text-muted-foreground" aria-hidden="true">
            …
          </span>
        ) : (
          <Button
            key={b}
            size="sm"
            variant={b === page ? 'default' : 'ghost'}
            aria-current={b === page ? 'page' : undefined}
            aria-label={`Page ${b + 1}`}
            data-faro-user-action-name="admin page"
            onClick={() => onPage(b)}
          >
            {b + 1}
          </Button>
        ),
      )}
      <Button size="sm" variant="ghost" disabled={page >= last} aria-label="Next page" onClick={() => onPage(page + 1)} data-faro-user-action-name="admin page next">
        <ChevronRight aria-hidden="true" />
      </Button>
    </nav>
  );
}

/** How each invite status shows: a live link stands out; the rest are quiet (none is an error). */
const STATUS_BADGE = {
  Active: 'default',
  'Turned off': 'outline',
  'Used up': 'secondary',
  Expired: 'outline',
} as const;

function InvitesTab({ allRoles }: { allRoles: RoleRef[] }) {
  const [invites, setInvites] = useState<Outcome<Schemas['AdminInvite'][]> | null>(null);
  const [round, setRound] = useState(0);
  useEffect(() => {
    void attempt(client.GET('/api/admin/invites')).then(setInvites);
  }, [round]);
  const reload = () => setRound((n) => n + 1);

  return (
    <div className="flex flex-col gap-6">
      <NewInviteForm allRoles={allRoles} made={reload} />
      {invites !== null && !invites.ok ? (
        <FormAlert message={invites.message} />
      ) : invites?.data.length === 0 ? (
        <EmptyCard>No invite links yet.</EmptyCard>
      ) : (
        <TableCard head={INVITES_HEAD} actions>
          {invites === null ? (
            <PlaceholderRows cells={INVITE_CELLS} rows={3} stacked={STACKED_WRAP} />
          ) : (
            invites.data.map((invite) => <InviteRow key={invite.id} invite={invite} changed={reload} />)
          )}
        </TableCard>
      )}
    </div>
  );
}

function NewInviteForm({ allRoles, made }: { allRoles: RoleRef[]; made: () => void }) {
  const ids = { role: useId(), note: useId(), days: useId(), uses: useId() };
  const [roleId, setRoleId] = useState(() => allRoles.find((r) => r.name === DEFAULT_INVITE_ROLE)?.id ?? 0);
  const [days, setDays] = useState('7');
  const [maxUses, setMaxUses] = useState('1');
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [link, setLink] = useState<string | null>(null);
  const [copied, setCopied] = useState<boolean | null>(null);

  const create = async () => {
    const form = newInvite({ roleId, days, maxUses, note });
    if (!form.ok) return setError(form.message);
    setBusy(true);
    setError(null);
    // So a failed attempt doesn't leave the previous link looking like the new one.
    setLink(null);
    const done = await attempt(client.POST('/api/admin/invites', { body: form.invite }));
    if (done.ok) {
      setLink(done.data.link ?? null);
      setCopied(null);
      setNote('');
      made();
    } else {
      setError(done.message);
    }
    setBusy(false);
  };
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(link ?? '');
      setCopied(true);
    } catch {
      setCopied(false);
    }
  };

  return (
    <Card>
      <form
        data-faro-user-action-name="admin create invite"
        className="flex flex-col gap-(--card-spacing)"
        onSubmit={(e) => {
          e.preventDefault();
          void create();
        }}
      >
        <CardHeader>
          <CardTitle>
            <h2>{INVITE_FORM.title}</h2>
          </CardTitle>
          <CardDescription>{INVITE_FORM.intro}</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 sm:grid-cols-2">
          <Field>
            <FieldLabel htmlFor={ids.role}>{INVITE_FORM.role}</FieldLabel>
            <NativeSelect id={ids.role} className="w-full" value={roleId} onChange={(e) => setRoleId(Number(e.target.value))}>
              {roleId === 0 && (
                <NativeSelectOption value={0} disabled>
                  Choose a role…
                </NativeSelectOption>
              )}
              {allRoles.map((r) => (
                <NativeSelectOption key={r.id} value={r.id}>
                  {r.name}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          </Field>
          <Field>
            <FieldLabel htmlFor={ids.note}>{INVITE_FORM.note}</FieldLabel>
            <Input
              id={ids.note}
              maxLength={100}
              placeholder="e.g. Sam, or the Discord group"
              value={note}
              onChange={(e) => setNote(e.target.value)}
            />
          </Field>
          <Field>
            <FieldLabel htmlFor={ids.days}>{INVITE_FORM.days}</FieldLabel>
            <Input id={ids.days} type="number" min={1} max={365} required value={days} onChange={(e) => setDays(e.target.value)} />
          </Field>
          <Field>
            <FieldLabel htmlFor={ids.uses}>{INVITE_FORM.uses}</FieldLabel>
            <Input id={ids.uses} type="number" min={1} max={1000} value={maxUses} onChange={(e) => setMaxUses(e.target.value)} />
          </Field>
        </CardContent>
        <CardFooter className="flex-col items-stretch gap-4">
          <Button type="submit" className="self-start" disabled={busy}>
            {INVITE_FORM.make}
          </Button>
          <FormAlert message={error} />
          {link && (
            <Alert>
              <CircleCheck aria-hidden="true" />
              <AlertDescription className="flex flex-col gap-2">
                <p>
                  Copy it now: it won't be shown again. Send it to the people you're inviting. Don't open it yourself while
                  logged in: that would use it.
                </p>
                {/* In the text colour: maia's input and outline button take the muted one around them. */}
                <div className="flex gap-2 text-foreground">
                  <Input readOnly aria-label="New invite link" className="font-mono" value={link} />
                  <Button type="button" variant="outline" onClick={() => void copy()} data-faro-user-action-name="admin copy invite link">
                    {copied ? 'Copied' : 'Copy'}
                  </Button>
                </div>
                {copied === false && <p>Couldn't copy it: select the link and copy it yourself.</p>}
              </AlertDescription>
            </Alert>
          )}
        </CardFooter>
      </form>
    </Card>
  );
}

function InviteRow({ invite, changed }: { invite: Schemas['AdminInvite']; changed: () => void }) {
  const [error, setError] = useState<string | null>(null);
  const status = inviteStatus(invite);
  const name = invite.note ?? `the ${invite.role} link`;
  const turnOff = async () => {
    const done = await attempt(client.DELETE('/api/admin/invites/{invite_id}', { params: { path: { invite_id: invite.id } } }));
    if (done.ok) changed();
    else setError(`Not turned off: ${done.message}`);
  };
  return (
    <TableRow role="row" className={STACKED_WRAP}>
      <TableCell role="cell">
        <div className="font-medium">{invite.note ?? '—'}</div>
        {invite.created_by && <div className="text-xs text-muted-foreground">by {invite.created_by}</div>}
        {invite.joined.length > 0 && <div className="text-xs text-muted-foreground">joined: {invite.joined.join(', ')}</div>}
      </TableCell>
      <TableCell role="cell">
        <Badge variant="secondary">{invite.role}</Badge>
      </TableCell>
      <TableCell role="cell" className="tabular-nums">{inviteUses(invite)}</TableCell>
      <TableCell role="cell" className="tabular-nums">{utcDate(invite.expires_at)}</TableCell>
      <TableCell role="cell">
        <Badge variant={STATUS_BADGE[status]}>{status}</Badge>
      </TableCell>
      <TableCell role="cell" className="text-right max-sm:ml-auto">
        {invite.live && (
          // Turning a link off can't be undone: it asks first.
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button size="sm" variant="ghost" aria-label={`Turn off ${name}`} data-faro-user-action-name="admin turn off invite">
                Turn off
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Turn off this link?</AlertDialogTitle>
                <AlertDialogDescription>
                  It stops letting anyone else in. People who joined through it keep the role. This can't be undone.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Keep</AlertDialogCancel>
                <AlertDialogAction variant="destructive" onClick={() => void turnOff()} data-faro-user-action-name="admin turn off invite confirmed">
                  Yes, turn off
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        )}
        {error && <p className="mt-1 text-xs text-destructive">{error}</p>}
      </TableCell>
    </TableRow>
  );
}
