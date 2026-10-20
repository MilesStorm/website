import { useEffect, useState } from 'react';
import { CircleCheck } from 'lucide-react';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { buttonVariants } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { FormAlert } from '@/components/FormAlert';
import { client } from '@/api/client';
import { attempt, type Outcome } from '@/lib/errors';
import { MISSING_CODE, linkCode } from '@/lib/login';

/** Confirming an email address from its link (`/verify-email#<code>`): opening it is enough, so
 * a link checker opening it too does no harm. */
export function VerifyEmail() {
  const [result, setResult] = useState<Outcome<{ username: string }> | null>(null);
  useEffect(() => {
    const code = linkCode();
    if (!code) return setResult({ ok: false, message: MISSING_CODE, code: null });
    void attempt(client.POST('/api/email/confirm', { body: { code } })).then(setResult);
  }, []);

  if (!result) return <Skeleton className="h-16 w-full" role="status" aria-label="Confirming" />;
  if (!result.ok) {
    return (
      <div className="flex flex-col gap-4">
        <FormAlert message={result.message} />
        <p className="text-sm text-muted-foreground">
          Your profile page shows whether your email is confirmed, and can send a new link.
        </p>
        <a href="/profile" className={buttonVariants({ variant: 'outline' })}>
          Open my profile
        </a>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-4">
      <Alert>
        <CircleCheck aria-hidden="true" />
        <AlertDescription>
          <p>
            Thanks! The email for <span className="font-semibold text-foreground">{result.data.username}</span> is
            confirmed.
          </p>
        </AlertDescription>
      </Alert>
      <a href="/" className={buttonVariants()}>
        Go to the site
      </a>
    </div>
  );
}
