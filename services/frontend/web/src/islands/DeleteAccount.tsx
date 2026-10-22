import { useEffect, useState } from 'react';
import { Button, buttonVariants } from '@/components/ui/button';
import { FormAlert } from '@/components/FormAlert';
import { bffUrl, client } from '@/api/client';
import { attempt, type Outcome } from '@/lib/errors';
import { DELETE_LINK } from '@/lib/copy';
import { MISSING_CODE, linkCode } from '@/lib/login';
import { DeleteLinkSkeleton } from './FormSkeleton';
import { island } from '@/lib/island';

/** Deleting an account from its emailed link (`/delete-account#<code>`): says whose account the
 * link is for, and deletes it only when the button is pressed. */
export const DeleteAccount = island('DeleteAccount', function DeleteAccount() {
  const [code] = useState(() => linkCode());
  const [account, setAccount] = useState<Outcome<{ username: string }> | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!code) return setAccount({ ok: false, message: MISSING_CODE, code: null });
    void attempt(client.POST('/api/deletion/check', { body: { code } })).then(setAccount);
  }, [code]);

  const remove = async () => {
    setBusy(true);
    setError(null);
    const done = await attempt(client.POST('/api/deletion/confirm', { body: { code } }));
    if (done.ok) return location.assign(bffUrl('/account-deleted'));
    setError(done.message);
    setBusy(false);
  };

  if (!account) return <DeleteLinkSkeleton />;
  if (!account.ok) {
    return (
      <div className="flex flex-col gap-4">
        <FormAlert message={account.message} />
        <p className="text-sm text-muted-foreground">You can ask for a new link on your profile page.</p>
        <a href="/profile" className={buttonVariants({ variant: 'outline' })}>
          Open my profile
        </a>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-4">
      <p>
        {DELETE_LINK.before} <span className="font-semibold">{account.data.username}</span>
        {DELETE_LINK.after}
      </p>
      <FormAlert message={error} />
      <div className="flex flex-col gap-2 sm:flex-row-reverse">
        <Button variant="destructive" className="sm:flex-1" disabled={busy} onClick={() => void remove()} data-faro-user-action-name="delete account link">
          Delete my account
        </Button>
        <a href="/" className={buttonVariants({ variant: 'outline', className: 'sm:flex-1' })}>
          Keep my account
        </a>
      </div>
    </div>
  );
});
