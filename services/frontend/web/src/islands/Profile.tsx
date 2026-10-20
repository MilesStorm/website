import { useEffect, useId, useRef, useState } from 'react';
import { CircleCheck } from 'lucide-react';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import { Field, FieldDescription, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { FormAlert } from '@/components/FormAlert';
import { bffUrl, client, type Schemas } from '@/api/client';
import { readBootstrap, viewerChanged, type Viewer } from '@/api/session';
import { attempt, type Outcome } from '@/lib/errors';
import defaultPicture from '@/assets/default-profile.png';

type Account = Schemas['AccountInfo'];

/** The server's own limits (src/account.rs, auth's display name). */
export const PICTURE_MAX_BYTES = 5 * 1024 * 1024;
export const NAME_MAX = 40;
const PICTURE_TYPES = 'image/jpeg,image/png,image/webp,image/gif';

/** What a section says after an action: done, or why not. */
type Note = { ok: boolean; text: string } | null;

function NoteLine({ note }: { note: Note }) {
  if (!note) return null;
  if (!note.ok) return <FormAlert message={note.text} />;
  return (
    <Alert>
      <CircleCheck aria-hidden="true" />
      <AlertDescription>{note.text}</AlertDescription>
    </Alert>
  );
}

/** The logged-in user's profile: name and picture, email, sharing roll pictures (with the
 * `arcane` permission), deleting the account. The theme is the page's own card. */
export function Profile() {
  const [account, setAccount] = useState<Outcome<Account> | null>(null);
  const [viewer, setViewer] = useState<Viewer | null>(null);
  useEffect(() => {
    void readBootstrap().then((b) => setViewer(b.viewer));
    void attempt(client.GET('/api/account')).then(setAccount);
  }, []);

  /** A new copy of the account from the server: the page and the user menu show it. */
  const update = (next: Account) => {
    setAccount({ ok: true, data: next });
    setViewer((v) => {
      if (!v) return v;
      const changed = { ...v, shown_name: next.shown_name, picture_url: next.picture_url };
      viewerChanged(changed);
      return changed;
    });
  };

  if (!account) return <Skeleton className="h-64 w-full rounded-xl" role="status" aria-label="Loading your account" />;
  if (!account.ok) return <FormAlert message={account.message} />;
  const canShare = viewer?.permissions.includes('arcane') ?? false;
  return (
    <div className="flex flex-col gap-6">
      <AccountCard account={account.data} update={update} />
      <EmailCard account={account.data} />
      {canShare && <SharingCard />}
      <DeleteCard account={account.data} />
    </div>
  );
}

function AccountCard({ account, update }: { account: Account; update: (a: Account) => void }) {
  const [name, setName] = useState(account.display_name ?? '');
  const [busy, setBusy] = useState(false);
  const [nameNote, setNameNote] = useState<Note>(null);
  const [pictureNote, setPictureNote] = useState<Note>(null);
  const file = useRef<HTMLInputElement>(null);
  const nameId = useId();
  const unchanged = name.trim() === (account.display_name ?? '');

  const saveName = async () => {
    setBusy(true);
    const done = await attempt(client.PUT('/api/account/display-name', { body: { name } }));
    if (done.ok) {
      update(done.data);
      setName(done.data.display_name ?? '');
    }
    setNameNote(done.ok ? { ok: true, text: 'Saved.' } : { ok: false, text: done.message });
    setBusy(false);
  };

  const upload = async (picture: File) => {
    if (picture.size > PICTURE_MAX_BYTES) return setPictureNote({ ok: false, text: 'That picture is too large (at most 5 MB).' });
    setBusy(true);
    setPictureNote(null);
    const done = await attempt(
      client.POST('/api/account/picture', {
        body: picture,
        bodySerializer: (body) => body as Blob,
        headers: { 'content-type': 'application/octet-stream' },
      }),
    );
    // The server says why a file was refused (not a picture it can read, too large).
    if (done.ok) {
      const fresh = await attempt(client.GET('/api/account'));
      if (fresh.ok) update(fresh.data);
    }
    setPictureNote(done.ok ? { ok: true, text: 'Picture updated.' } : { ok: false, text: done.message });
    setBusy(false);
  };

  const remove = async () => {
    setBusy(true);
    const done = await attempt(client.DELETE('/api/account/picture'));
    if (done.ok) update(done.data);
    setPictureNote(done.ok ? { ok: true, text: 'Picture removed.' } : { ok: false, text: done.message });
    setBusy(false);
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2>Account</h2>
        </CardTitle>
        <CardDescription>How you appear on this site.</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-8 sm:grid-cols-[auto_1fr]">
        <div className="flex flex-col items-center gap-3 sm:items-start">
          <img
            src={account.picture_url ? bffUrl(account.picture_url) : defaultPicture.src}
            alt="Your profile picture"
            width={96}
            height={96}
            className="size-24 rounded-full object-cover ring-2 ring-border"
          />
          {account.pictures_available ? (
            <>
              <div className="flex gap-2">
                <Button size="sm" disabled={busy} onClick={() => file.current?.click()}>
                  Upload
                </Button>
                {account.picture_version !== null && (
                  <Button size="sm" variant="outline" disabled={busy} onClick={() => void remove()}>
                    Remove
                  </Button>
                )}
              </div>
              <input
                ref={file}
                type="file"
                accept={PICTURE_TYPES}
                className="sr-only"
                tabIndex={-1}
                aria-label="Profile picture file"
                onChange={(e) => {
                  const picture = e.target.files?.[0];
                  // Cleared, so choosing the same file again uploads it again.
                  e.target.value = '';
                  if (picture) void upload(picture);
                }}
              />
              <p className="text-xs text-muted-foreground">JPEG, PNG, WebP or GIF, up to 5 MB.</p>
            </>
          ) : (
            <p className="max-w-40 text-center text-xs text-muted-foreground sm:text-left">
              Pictures can't be changed right now.
            </p>
          )}
          <NoteLine note={pictureNote} />
        </div>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void saveName();
          }}
          className="flex flex-col gap-3"
        >
          <Field>
            <FieldLabel htmlFor={nameId}>Display name</FieldLabel>
            <div className="flex flex-wrap gap-2">
              <Input
                id={nameId}
                className="max-w-xs"
                placeholder={account.username}
                maxLength={NAME_MAX}
                value={name}
                disabled={busy}
                aria-describedby={`${nameId}-description`}
                onChange={(e) => {
                  setName(e.target.value);
                  setNameNote(null);
                }}
              />
              <Button type="submit" disabled={busy || unchanged}>
                Save
              </Button>
            </div>
            <FieldDescription id={`${nameId}-description`}>
              Shown on the site instead of your username. Leave it empty to use your username,{' '}
              <span className="font-semibold">{account.username}</span>, which you log in with and which can't be
              changed.
            </FieldDescription>
          </Field>
          <NoteLine note={nameNote} />
        </form>
      </CardContent>
    </Card>
  );
}

function EmailCard({ account }: { account: Account }) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<Note>(null);
  const unconfirmed = account.email !== null && !account.email_verified;

  const send = async () => {
    setBusy(true);
    const done = await attempt(client.POST('/api/account/email/send-confirmation'));
    setNote(
      done.ok
        ? { ok: true, text: `Sent. Open the link in the email to ${done.data.email}; check your spam folder too.` }
        : { ok: false, text: done.message },
    );
    setBusy(false);
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2>Email</h2>
        </CardTitle>
        {account.email === null ? (
          <CardDescription>Your account has no email address: you log in with GitHub.</CardDescription>
        ) : (
          <CardDescription className="flex flex-wrap items-center gap-2">
            <span className="break-all text-foreground">{account.email}</span>
            {account.email_verified ? <Badge>Confirmed</Badge> : <Badge variant="outline">Not confirmed</Badge>}
          </CardDescription>
        )}
        {unconfirmed && (
          <CardAction>
            <Button size="sm" disabled={busy} onClick={() => void send()}>
              Send confirmation email
            </Button>
          </CardAction>
        )}
      </CardHeader>
      {(unconfirmed || note) && (
        <CardContent className="flex flex-col gap-3">
          {unconfirmed && (
            <p className="text-sm text-muted-foreground">Confirm it so we can help you if you forget your password.</p>
          )}
          <NoteLine note={note} />
        </CardContent>
      )}
    </Card>
  );
}

function SharingCard() {
  const [state, setState] = useState<Outcome<Schemas['SharingState']> | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [note, setNote] = useState<Note>(null);
  useEffect(() => {
    void attempt(client.GET('/api/account/sharing')).then(setState);
  }, []);

  if (!state) return <Skeleton className="h-40 w-full rounded-xl" role="status" aria-label="Loading your sharing setting" />;
  if (!state.ok) return <FormAlert message="Couldn't load your sharing setting." />;
  // The store isn't set up on this server: nothing to choose.
  if (!state.data.available) return null;

  const choose = async (share: boolean) => {
    setBusy(true);
    const done = await attempt(client.PUT('/api/account/sharing', { body: { share } }));
    if (done.ok) setState(done);
    setNote(done.ok ? null : { ok: false, text: "Couldn't save your choice. Try again." });
    setBusy(false);
  };
  const removeAll = async () => {
    setBusy(true);
    const done = await attempt(client.DELETE('/api/account/sharing/data'));
    if (done.ok) {
      setState({ ok: true, data: { available: true, share: false } });
      const n = done.data.deleted;
      setNote({ ok: true, text: n === 0 ? 'Done. There was nothing to delete.' : n === 1 ? 'Deleted 1 roll.' : `Deleted ${n} rolls.` });
    } else {
      setNote({ ok: false, text: "Couldn't delete right now. Try again." });
    }
    setConfirming(false);
    setBusy(false);
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2>Help improve dice recognition</h2>
        </CardTitle>
        <CardDescription>Share pictures of your rolls so the dice reader gets better at reading them.</CardDescription>
        <CardAction>
          <Switch
            checked={state.data.share}
            disabled={busy}
            aria-label="Share pictures of my rolls"
            onCheckedChange={(share) => void choose(share)}
          />
        </CardAction>
      </CardHeader>
      <CardContent>
        <dl className="grid gap-5 sm:grid-cols-3">
          <div>
            <dt className="text-xs font-semibold tracking-wide text-muted-foreground uppercase">What's saved</dt>
            <dd className="mt-1 text-sm">The camera picture and what the model read.</dd>
          </div>
          <div>
            <dt className="text-xs font-semibold tracking-wide text-muted-foreground uppercase">Which rolls</dt>
            <dd className="mt-1 text-sm">Rolls the model was unsure about, and about 1 in 20 others.</dd>
          </div>
          <div>
            <dt className="text-xs font-semibold tracking-wide text-muted-foreground uppercase">Who sees them</dt>
            <dd className="mt-1 text-sm">Only the site owner, to check the numbers. Used only to train the dice reader.</dd>
          </div>
        </dl>
      </CardContent>
      <CardFooter className="flex flex-col items-stretch gap-3">
        {confirming ? (
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="text-sm">Delete every roll picture you've shared or flagged? This also turns sharing off.</p>
            <div className="flex gap-2">
              <Button size="sm" variant="ghost" onClick={() => setConfirming(false)}>
                Cancel
              </Button>
              <Button size="sm" variant="destructive" disabled={busy} onClick={() => void removeAll()}>
                Delete everything
              </Button>
            </div>
          </div>
        ) : (
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="max-w-lg text-xs text-muted-foreground">
              Turning this off stops new saves. Rolls you flag as wrong in the browser extension are saved either way;
              your latest roll's picture is kept for 10 minutes so it can be flagged.
            </p>
            <Button size="sm" variant="ghost" onClick={() => setConfirming(true)}>
              Delete my shared pictures
            </Button>
          </div>
        )}
        <NoteLine note={note} />
      </CardFooter>
    </Card>
  );
}

function DeleteCard({ account }: { account: Account }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [typed, setTyped] = useState('');
  const [note, setNote] = useState<Note>(null);
  const confirmId = useId();
  // A confirmed address gets a link; otherwise it might never arrive.
  const byEmail = account.email !== null && account.email_verified;
  const unconfirmed = account.email !== null && !account.email_verified;

  const close = () => {
    setOpen(false);
    setTyped('');
    setNote(null);
  };
  const sendLink = async () => {
    setBusy(true);
    const done = await attempt(client.POST('/api/account/deletion/request'));
    setNote(done.ok ? { ok: true, text: `Sent to ${done.data.email}. The link works for 1 hour.` } : { ok: false, text: done.message });
    setBusy(false);
  };
  const deleteNow = async () => {
    setBusy(true);
    const done = await attempt(client.POST('/api/account/deletion/direct', { body: { confirm: typed.trim() } }));
    if (done.ok) return location.assign(bffUrl('/account-deleted'));
    setNote({ ok: false, text: done.message });
    setBusy(false);
  };

  return (
    <Card className="border-destructive/40">
      <CardHeader>
        <CardTitle>
          <h2>Delete account</h2>
        </CardTitle>
        <CardDescription>
          Removes your account and profile picture. Any data you chose to share will also be removed. This can't be
          undone.
        </CardDescription>
        {!open && (
          <CardAction>
            <Button size="sm" variant="destructive" onClick={() => setOpen(true)}>
              Delete my account…
            </Button>
          </CardAction>
        )}
      </CardHeader>
      {open && (
        <CardContent className="flex flex-col gap-3">
          {byEmail ? (
            <>
              <p className="text-sm">
                We'll email you a link to confirm. Nothing is deleted until you open it and confirm once more.
              </p>
              <div className="flex flex-wrap gap-2">
                <Button size="sm" variant="ghost" onClick={close}>
                  Cancel
                </Button>
                <Button size="sm" variant="destructive" disabled={busy} onClick={() => void sendLink()}>
                  Email me the link
                </Button>
              </div>
            </>
          ) : (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void deleteNow();
              }}
              className="flex flex-col gap-3"
            >
              <Field>
                <FieldLabel htmlFor={confirmId}>
                  {unconfirmed && "Your email isn't confirmed, so there's no link to wait for. "}Type your username,{' '}
                  {account.username}, to confirm.
                </FieldLabel>
                <Input
                  id={confirmId}
                  className="max-w-xs"
                  autoComplete="off"
                  value={typed}
                  onChange={(e) => setTyped(e.target.value)}
                />
              </Field>
              <div className="flex flex-wrap gap-2">
                <Button type="button" size="sm" variant="ghost" onClick={close}>
                  Cancel
                </Button>
                <Button type="submit" size="sm" variant="destructive" disabled={busy || typed.trim() !== account.username}>
                  Delete permanently
                </Button>
              </div>
            </form>
          )}
          <NoteLine note={note} />
        </CardContent>
      )}
    </Card>
  );
}
