import { Fragment, useEffect, useId, useRef, useState } from 'react';
import { ChevronDown, CircleCheck } from 'lucide-react';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible';
import { Field, FieldDescription, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { AppearanceCard } from '@/components/Appearance';
import { FormAlert } from '@/components/FormAlert';
import { SECTIONS, Section, Setting } from '@/components/Setting';
import { bffUrl, client, type Schemas } from '@/api/client';
import { logOut, readBootstrap, viewerChanged, type Viewer } from '@/api/session';
import { attempt, type Outcome } from '@/lib/errors';
import {
  ACCOUNT_INTRO,
  DANGER_INTRO,
  DELETE_INTRO,
  LOGOUT_INTRO,
  NAME_HINT,
  PICTURE_HINT,
  SESSION_INTRO,
  SHARING_DELETE_INTRO,
  SHARING_DELETE_TITLE,
  SHARING_FACTS_INTRO,
  SHARING_FACTS_TITLE,
  SHARING_INTRO,
  SHARING_NOTE,
  SHARING_SECTION_INTRO,
  SHARING_SWITCH,
  SHARING_TITLE,
  USERNAME_HINT,
} from '@/lib/copy';
import { DELETE_OPEN_LABEL, ProfileSkeleton, SHARING_DELETE_LABEL, SharingFacts, SharingSkeleton } from './ProfileSkeleton';
import defaultPicture from '@/assets/default-profile.png';
import { island } from '@/lib/island';

type Account = Schemas['AccountInfo'];

/** The server's own limits (src/account.rs, auth's display name). */
export const PICTURE_MAX_BYTES = 5 * 1024 * 1024;
export const NAME_MAX = 40;
const PICTURE_TYPES = 'image/jpeg,image/png,image/webp,image/gif';
/** The outline button of an action that deletes: red text and edge, a red wash on hover. */
const DANGER_OUTLINE = 'border-destructive/40 text-destructive hover:bg-destructive/10 hover:text-destructive';

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

/** The logged-in user's profile: name and picture, email, how the site looks, sharing roll pictures
 * (with the `arcane` permission), logging out and deleting the account. */
export const Profile = island('Profile', function Profile() {
  const [account, setAccount] = useState<Outcome<Account> | null>(null);
  const [viewer, setViewer] = useState<Viewer | null>(null);
  useEffect(() => {
    void readBootstrap().then((b) => setViewer(b.viewer));
    void attempt(client.GET('/api/account')).then(setAccount);
  }, []);

  /** A new copy of the account from the server: the page and the user menu show it. */
  const update = (next: Account) => {
    setAccount({ ok: true, data: next });
    if (!viewer) return;
    const changed = { ...viewer, shown_name: next.shown_name, picture_url: next.picture_url };
    setViewer(changed);
    viewerChanged(changed);
  };

  if (!account) return <ProfileSkeleton />;
  if (!account.ok) {
    return (
      // The alert above the sections, not one of them (no divider, no section's padding).
      <div className="flex max-w-5xl flex-col gap-8">
        <FormAlert message={account.message} />
        <div className={SECTIONS}>
          <AppearanceCard />
          <SessionCard />
        </div>
      </div>
    );
  }
  const canShare = viewer?.permissions.includes('arcane') ?? false;
  return (
    <div className={SECTIONS}>
      <AccountCard account={account.data} update={update} />
      <AppearanceCard />
      {canShare && <SharingCard />}
      <SessionCard account={account.data} />
    </div>
  );
});

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
    <Section title="Account" description={ACCOUNT_INTRO}>
      <Setting
        title="Profile picture"
        description={account.pictures_available ? PICTURE_HINT : "Pictures can't be changed right now."}
        lead={
          <img
            src={account.picture_url ? bffUrl(account.picture_url) : defaultPicture.src}
            alt="Your profile picture"
            width={48}
            height={48}
            className="size-12 shrink-0 rounded-full object-cover ring-2 ring-border"
          />
        }
        note={pictureNote && <NoteLine note={pictureNote} />}
      >
        {account.pictures_available && (
          <>
            <Button
              size="sm"
              variant="outline"
              disabled={busy}
              onClick={() => file.current?.click()}
              data-faro-user-action-name="profile upload picture"
            >
              Change
            </Button>
            {account.picture_version !== null && (
              <Button size="sm" variant="outline" disabled={busy} onClick={() => void remove()} data-faro-user-action-name="profile remove picture">
                Remove
              </Button>
            )}
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
          </>
        )}
      </Setting>
      <Setting
        wide
        title="Display name"
        htmlFor={nameId}
        description={NAME_HINT}
        descriptionId={`${nameId}-description`}
        note={nameNote && <NoteLine note={nameNote} />}
      >
        <form
          data-faro-user-action-name="profile save name"
          onSubmit={(e) => {
            e.preventDefault();
            void saveName();
          }}
          className="flex w-full gap-2 @lg:w-auto"
        >
          <Input
            id={nameId}
            className="min-w-0 flex-1 @lg:w-52 @lg:flex-none"
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
          <Button type="submit" variant={unchanged ? 'outline' : 'default'} disabled={busy || unchanged}>
            Save
          </Button>
        </form>
      </Setting>
      <Setting title="Username" description={USERNAME_HINT}>
        <span className="text-sm font-medium">{account.username}</span>
      </Setting>
      <EmailSetting account={account} />
    </Section>
  );
}

function EmailSetting({ account }: { account: Account }) {
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
    <div>
      {/* The label with its badge, then the address on a line of its own: it wraps after the @ and
          the dots (a break opportunity, never mid-word), and only a part longer than the line breaks. */}
      <div className="grid gap-x-4 gap-y-1 @lg:grid-cols-[minmax(0,1fr)_auto] @lg:items-center">
        <div className="flex min-w-0 items-center gap-2">
          <span className="text-sm font-medium">Email</span>
          {account.email !== null && (account.email_verified ? <Badge>Confirmed</Badge> : <Badge variant="outline">Not confirmed</Badge>)}
        </div>
        {account.email === null ? (
          <p className="text-sm text-muted-foreground @lg:col-span-2">Your account has no email address: you log in with GitHub.</p>
        ) : (
          <>
            <p className="text-sm [overflow-wrap:anywhere] @lg:col-span-2">{breakable(account.email)}</p>
            {unconfirmed && (
              <p className="text-sm text-muted-foreground @lg:col-span-2">Confirm it so we can help you if you forget your password.</p>
            )}
          </>
        )}
        {unconfirmed && (
          <Button
            size="sm"
            variant="outline"
            className="mt-2 @max-lg:w-full @lg:col-start-2 @lg:row-start-1 @lg:mt-0"
            disabled={busy}
            onClick={() => void send()}
            data-faro-user-action-name="profile send confirmation"
          >
            Send confirmation email
          </Button>
        )}
      </div>
      {note && (
        <div className="mt-4 flex flex-col gap-3">
          <NoteLine note={note} />
        </div>
      )}
    </div>
  );
}

/** An address with a break opportunity after its @ and each dot, so a long one wraps between
 * its parts instead of in the middle of one. */
function breakable(address: string) {
  return (address.match(/[^@.]+[@.]?|[@.]/g) ?? [address]).map((part, i) => (
    <Fragment key={i}>
      {i > 0 && <wbr />}
      {part}
    </Fragment>
  ));
}

function SharingCard() {
  const [state, setState] = useState<Outcome<Schemas['SharingState']> | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [facts, setFacts] = useState(false);
  const [note, setNote] = useState<Note>(null);
  const switchId = useId();
  useEffect(() => {
    void attempt(client.GET('/api/account/sharing')).then(setState);
  }, []);

  // Shaped for the usual answer (the store is set up): the rarer ones below are shorter.
  if (!state) {
    return (
      <div role="status" aria-label="Loading your sharing setting" aria-busy="true">
        <SharingSkeleton />
      </div>
    );
  }
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
    <Section title={SHARING_TITLE} description={SHARING_SECTION_INTRO}>
      <Setting title={SHARING_SWITCH} htmlFor={switchId} description={SHARING_INTRO}>
        <Switch id={switchId} checked={state.data.share} disabled={busy} onCheckedChange={(share) => void choose(share)} />
      </Setting>
      <Collapsible open={facts} onOpenChange={setFacts}>
        <Setting title={SHARING_FACTS_TITLE} description={SHARING_FACTS_INTRO}>
          <CollapsibleTrigger asChild>
            <Button size="sm" variant="ghost">
              {facts ? 'Hide' : 'Show'} <ChevronDown className={facts ? 'rotate-180' : ''} />
            </Button>
          </CollapsibleTrigger>
        </Setting>
        <CollapsibleContent className="flex flex-col gap-3 pt-4">
          <SharingFacts />
          <p className="text-xs text-muted-foreground">{SHARING_NOTE}</p>
        </CollapsibleContent>
      </Collapsible>
      <Setting
        title={SHARING_DELETE_TITLE}
        description={
          confirming ? "Delete every roll picture you've shared or flagged? This also turns sharing off." : SHARING_DELETE_INTRO
        }
        note={note && <NoteLine note={note} />}
      >
        {confirming ? (
          <>
            <Button size="sm" variant="ghost" onClick={() => setConfirming(false)}>
              Cancel
            </Button>
            <Button size="sm" variant="destructive" disabled={busy} onClick={() => void removeAll()} data-faro-user-action-name="profile delete data">
              Delete everything
            </Button>
          </>
        ) : (
          <Button size="sm" variant="outline" className={DANGER_OUTLINE} onClick={() => setConfirming(true)}>
            {SHARING_DELETE_LABEL}
          </Button>
        )}
      </Setting>
    </Section>
  );
}

/** Logging out, and (when the account is known) the danger zone: deleting it. */
function SessionCard({ account }: { account?: Account }) {
  return (
    <>
      <Section title="Session" description={SESSION_INTRO}>
        <Setting title="Log out" description={LOGOUT_INTRO}>
          <Button size="sm" variant="outline" onClick={() => void logOut()} data-faro-user-action-name="profile log out">
            Log out
          </Button>
        </Setting>
      </Section>
      {account && (
        <Section danger title="Danger zone" description={DANGER_INTRO}>
          <DeleteSetting account={account} />
        </Section>
      )}
    </>
  );
}

function DeleteSetting({ account }: { account: Account }) {
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
    <Setting
      title="Delete account"
      description={DELETE_INTRO}
      note={
        open && (
          <>
            {byEmail ? (
              <>
                <p className="text-sm">
                  We'll email you a link to confirm. Nothing is deleted until you open it and confirm once more.
                </p>
                <div className="flex flex-wrap justify-end gap-2">
                  <Button size="sm" variant="ghost" onClick={close}>
                    Cancel
                  </Button>
                  <Button size="sm" variant="destructive" disabled={busy} onClick={() => void sendLink()} data-faro-user-action-name="profile email delete link">
                    Email me the link
                  </Button>
                </div>
              </>
            ) : (
              <form
                data-faro-user-action-name="profile delete account"
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
                <div className="flex flex-wrap justify-end gap-2">
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
          </>
        )
      }
    >
      {!open && (
        <Button size="sm" variant="destructive" onClick={() => setOpen(true)}>
          {DELETE_OPEN_LABEL}
        </Button>
      )}
    </Setting>
  );
}
