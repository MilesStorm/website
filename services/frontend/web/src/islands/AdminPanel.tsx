import { useCallback, useEffect, useId, useState } from 'react';
import { ChevronLeft, ChevronRight, CircleCheck, X } from 'lucide-react';
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
import { Field, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { NativeSelect, NativeSelectOption } from '@/components/ui/native-select';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { FormAlert } from '@/components/FormAlert';
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

type RoleRef = Schemas['AdminRoleRef'];
type Permission = Schemas['AdminPermission'];

/** How long typing in a search box waits before asking. */
export const SEARCH_DELAY_MS = 250;

/** The admin panel: users and their roles, roles and their permissions, invite links. */
export function AdminPanel() {
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

  if (!roles || !permissions) return <Skeleton className="h-96 w-full rounded-xl" role="status" aria-label="Loading" />;
  const allRoles = roles.ok ? roles.data : [];
  const allPermissions = permissions.ok ? permissions.data : [];
  return (
    <div className="flex flex-col gap-4">
      {!roles.ok && <FormAlert message={`Roles: ${roles.message}`} />}
      {!permissions.ok && <FormAlert message={`Permissions: ${permissions.message}`} />}
      <Tabs defaultValue="users">
        <TabsList>
          <TabsTrigger value="users">Users</TabsTrigger>
          <TabsTrigger value="roles">Roles</TabsTrigger>
          <TabsTrigger value="invites">Invites</TabsTrigger>
        </TabsList>
        <TabsContent value="users">
          <UsersTab allRoles={allRoles} me={me} />
        </TabsContent>
        <TabsContent value="roles">
          <RolesTab allPermissions={allPermissions} />
        </TabsContent>
        <TabsContent value="invites">
          <InvitesTab allRoles={allRoles} />
        </TabsContent>
      </Tabs>
    </div>
  );
}

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
      head={['Username', 'Email', 'Roles']}
      row={(user) => (
        <TableRow key={user.id}>
          <TableCell className="font-medium">{user.username}</TableCell>
          <TableCell className="text-muted-foreground">{user.email ?? '—'}</TableCell>
          <TableCell>
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
      head={['Role', 'Permissions']}
      row={(role) => (
        <TableRow key={role.id}>
          <TableCell className="font-medium">{role.name}</TableCell>
          <TableCell>
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

/** A search box, the count, the table and the pager of a listing. */
function Listing<T>({
  label,
  placeholder,
  noun,
  list,
  error,
  head,
  row,
}: {
  label: string;
  placeholder: string;
  noun: string;
  list: ReturnType<typeof useListing<T>>;
  error: string | null;
  head: string[];
  row: (item: T) => React.ReactNode;
}) {
  const id = useId();
  const result = list.result;
  const total = result?.ok ? result.data.total : 0;
  return (
    <div className="flex flex-col gap-3 pt-2">
      <div className="flex flex-wrap items-center gap-3">
        <label htmlFor={id} className="sr-only">
          {label}
        </label>
        <Input
          id={id}
          type="search"
          className="max-w-sm"
          placeholder={placeholder}
          value={list.search}
          onChange={(e) => list.setSearch(e.target.value)}
        />
        {result?.ok && (
          <span className="shrink-0 text-sm text-muted-foreground" aria-live="polite">
            {total} {noun}
            {total === 1 ? '' : 's'}
          </span>
        )}
      </div>
      <FormAlert message={error} />
      {result === null ? (
        <Skeleton className="h-48 w-full" role="status" aria-label="Loading" />
      ) : !result.ok ? (
        <FormAlert message={result.message} />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              {head.map((h) => (
                <TableHead key={h}>{h}</TableHead>
              ))}
            </TableRow>
          </TableHeader>
          <TableBody>{result.data.items.map(row)}</TableBody>
        </Table>
      )}
      <Pager page={list.page} total={total} onPage={list.setPage} />
    </div>
  );
}

/** Why removing one of the viewer's own roles asks first. */
export const OWN_ROLE =
  "This is your own account. If this role is what gives you admin rights, you'll lose access to this page and can't give it back yourself.";
/** Why taking `manage_permissions` off a role asks first. */
export const MANAGE_PERMISSIONS =
  "manage_permissions is what lets anyone use this page. If your account has admin rights only through this role, you'll lose access to it and can't give it back yourself.";

/** What a user or role has (each removable), and a picker to add one more. A permission auth
 * has and this site doesn't know is marked, since nothing here checks it. A removal `warn` has
 * a reason for asks first. */
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
  const [chosen, setChosen] = useState<number | null>(null);
  const pick = chosen !== null && missing.some((m) => m.id === chosen) ? chosen : (missing[0]?.id ?? null);
  const unknown = (name: string) => kind === 'permission' && !isKnownPermission(name);
  return (
    <div className="flex flex-wrap items-center gap-1">
      {have.map((h) => (
        <Badge
          key={h.id}
          variant={unknown(h.name) ? 'outline' : 'secondary'}
          title={unknown(h.name) ? 'A permission this site doesn’t use yet' : undefined}
        >
          {h.name}
          {unknown(h.name) && <span>(unused)</span>}
          <RemoveButton
            label={`Remove ${kind} ${h.name} from ${owner}`}
            title={`Remove ${h.name} from ${owner}?`}
            warning={warn(h)}
            remove={() => remove(h.id)}
          />
        </Badge>
      ))}
      {missing.length > 0 && pick !== null && (
        <span className="flex items-center gap-1">
          <NativeSelect
            size="sm"
            aria-label={`${kind === 'role' ? 'Role' : 'Permission'} to add to ${owner}`}
            value={pick}
            onChange={(e) => setChosen(Number(e.target.value))}
          >
            {missing.map((m) => (
              <NativeSelectOption key={m.id} value={m.id}>
                {m.name}
                {unknown(m.name) ? ' (unused)' : ''}
              </NativeSelectOption>
            ))}
          </NativeSelect>
          <Button size="sm" variant="outline" aria-label={`Add ${kind} to ${owner}`} onClick={() => add(pick)}>
            Add
          </Button>
        </span>
      )}
    </div>
  );
}

/** A badge's remove button; with a `warning`, it asks first and says why. */
function RemoveButton({ label, title, warning, remove }: { label: string; title: string; warning: string | null; remove: () => void }) {
  const button = (
    <button type="button" className="-mr-1 rounded-sm p-0.5 hover:bg-muted" aria-label={label} onClick={warning ? undefined : remove}>
      <X aria-hidden="true" />
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
          <AlertDialogAction variant="destructive" onClick={remove}>
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
      <Button size="sm" variant="ghost" disabled={page === 0} aria-label="Previous page" onClick={() => onPage(page - 1)}>
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
            onClick={() => onPage(b)}
          >
            {b + 1}
          </Button>
        ),
      )}
      <Button size="sm" variant="ghost" disabled={page >= last} aria-label="Next page" onClick={() => onPage(page + 1)}>
        <ChevronRight aria-hidden="true" />
      </Button>
    </nav>
  );
}

function InvitesTab({ allRoles }: { allRoles: RoleRef[] }) {
  const [invites, setInvites] = useState<Outcome<Schemas['AdminInvite'][]> | null>(null);
  const [round, setRound] = useState(0);
  useEffect(() => {
    void attempt(client.GET('/api/admin/invites')).then(setInvites);
  }, [round]);
  const reload = () => setRound((n) => n + 1);

  return (
    <div className="flex flex-col gap-6 pt-2">
      <NewInviteForm allRoles={allRoles} made={reload} />
      {invites === null ? (
        <Skeleton className="h-48 w-full" role="status" aria-label="Loading" />
      ) : !invites.ok ? (
        <FormAlert message={invites.message} />
      ) : invites.data.length === 0 ? (
        <p className="text-sm text-muted-foreground">No invite links yet.</p>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Note</TableHead>
              <TableHead>Role</TableHead>
              <TableHead>Used</TableHead>
              <TableHead>Until (UTC)</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {invites.data.map((invite) => (
              <InviteRow key={invite.id} invite={invite} changed={reload} />
            ))}
          </TableBody>
        </Table>
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
    <form
      className="flex flex-col gap-4 rounded-xl border p-4"
      onSubmit={(e) => {
        e.preventDefault();
        void create();
      }}
    >
      <h2 className="font-semibold">New invite link</h2>
      <div className="grid gap-4 sm:grid-cols-2">
        <Field>
          <FieldLabel htmlFor={ids.role}>Gives the role</FieldLabel>
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
          <FieldLabel htmlFor={ids.note}>Note (who it's for)</FieldLabel>
          <Input
            id={ids.note}
            maxLength={100}
            placeholder="e.g. Sam, or the Discord group"
            value={note}
            onChange={(e) => setNote(e.target.value)}
          />
        </Field>
        <Field>
          <FieldLabel htmlFor={ids.days}>Works for (days)</FieldLabel>
          <Input id={ids.days} type="number" min={1} max={365} required value={days} onChange={(e) => setDays(e.target.value)} />
        </Field>
        <Field>
          <FieldLabel htmlFor={ids.uses}>How many people can use it (empty = no limit)</FieldLabel>
          <Input id={ids.uses} type="number" min={1} max={1000} value={maxUses} onChange={(e) => setMaxUses(e.target.value)} />
        </Field>
      </div>
      <Button type="submit" className="self-start" disabled={busy}>
        Make link
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
              <Button type="button" variant="outline" onClick={() => void copy()}>
                {copied ? 'Copied' : 'Copy'}
              </Button>
            </div>
            {copied === false && <p>Couldn't copy it: select the link and copy it yourself.</p>}
          </AlertDescription>
        </Alert>
      )}
    </form>
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
    <TableRow>
      <TableCell>
        <div className="font-medium">{invite.note ?? '—'}</div>
        {invite.created_by && <div className="text-xs text-muted-foreground">by {invite.created_by}</div>}
        {invite.joined.length > 0 && <div className="text-xs text-muted-foreground">joined: {invite.joined.join(', ')}</div>}
      </TableCell>
      <TableCell>
        <Badge variant="secondary">{invite.role}</Badge>
      </TableCell>
      <TableCell>{inviteUses(invite)}</TableCell>
      <TableCell>{utcDate(invite.expires_at)}</TableCell>
      <TableCell>
        <Badge variant={status === 'Active' ? 'default' : 'outline'}>{status}</Badge>
      </TableCell>
      <TableCell>
        {invite.live && (
          // Turning a link off can't be undone: it asks first.
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button size="sm" variant="ghost" aria-label={`Turn off ${name}`}>
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
                <AlertDialogAction variant="destructive" onClick={() => void turnOff()}>
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
