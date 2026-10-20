import { useEffect, useState } from 'react';
import { CircleAlert } from 'lucide-react';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button, buttonVariants } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { FormAlert } from '@/components/FormAlert';
import { bffUrl, client } from '@/api/client';
import { attempt, type Outcome } from '@/lib/errors';
import { MISSING_CODE, linkCode } from '@/lib/login';

/** Deleting an account from its emailed link (`/delete-account#<code>`): says whose account the
 * link is for, and deletes it only when the button is pressed. */
export function DeleteAccount() {
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

  if (!account) return <Skeleton className="h-24 w-full" aria-label="Checking the link" />;
  if (!account.ok) {
    return (
      <div className="flex flex-col gap-4">
        <Alert variant="destructive">
          <CircleAlert aria-hidden="true" />
          <AlertDescription>{account.message}</AlertDescription>
        </Alert>
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
        This permanently deletes the account <span className="font-semibold">{account.data.username}</span>: your
        profile and profile picture. Any data you chose to share will also be removed. It can't be undone.
      </p>
      <FormAlert message={error} />
      <div className="flex flex-col gap-2 sm:flex-row-reverse">
        <Button variant="destructive" className="sm:flex-1" disabled={busy} onClick={() => void remove()}>
          Delete my account
        </Button>
        <a href="/" className={buttonVariants({ variant: 'outline', className: 'sm:flex-1' })}>
          Keep my account
        </a>
      </div>
    </div>
  );
}
