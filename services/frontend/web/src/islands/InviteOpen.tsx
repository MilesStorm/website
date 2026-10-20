import { useEffect, useState } from 'react';
import { CircleAlert, CircleCheck } from 'lucide-react';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { buttonVariants } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { client, type Schemas } from '@/api/client';
import { applySession, fetchBootstrap } from '@/api/session';
import { attempt, type Outcome } from '@/lib/errors';
import { MISSING_CODE, linkCode } from '@/lib/login';
import type { Href } from '@/lib/pages';

/** Where to go after joining a role, and the button that goes there. */
export function afterJoin(role: string): [Href, string] {
  return role === 'arcane_user' ? ['/arcane', 'Open Arcane dice'] : ['/', 'Go to the site'];
}

/** The page an invite link opens (`/invite#<code>`). Logged in, it joins at once. Logged out, the
 * server keeps the invite: logging in or signing up joins, and the login comes back here. */
export function InviteOpen() {
  const [opened, setOpened] = useState<Outcome<Schemas['Opened']> | null>(null);
  useEffect(() => {
    const code = linkCode();
    if (!code) return setOpened({ ok: false, message: MISSING_CODE, code: null });
    void attempt(client.POST('/api/invites/open', { body: { code } })).then(async (result) => {
      // The new pages (Arcane in the menu) show at once.
      if (result.ok && result.data.kind === 'joined') applySession(await fetchBootstrap());
      setOpened(result);
    });
  }, []);

  if (!opened) return <Skeleton className="h-24 w-full" aria-label="Opening the invite" />;
  if (!opened.ok) {
    return (
      <div className="flex flex-col gap-4">
        <Alert variant="destructive">
          <CircleAlert aria-hidden="true" />
          <AlertDescription>{opened.message}</AlertDescription>
        </Alert>
        <a href="/" className={buttonVariants({ variant: 'outline' })}>
          Go to the site
        </a>
      </div>
    );
  }
  const { kind, role, group_label } = opened.data;
  if (kind === 'joined') {
    const [page, label] = afterJoin(role);
    return (
      <div className="flex flex-col gap-4">
        <Alert>
          <CircleCheck aria-hidden="true" />
          <AlertDescription>You're in! You've joined {group_label}.</AlertDescription>
        </Alert>
        <a href={page} className={buttonVariants()}>
          {label}
        </a>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-4">
      <p className="text-center">
        You've been invited to join <span className="font-semibold">{group_label}</span>.
      </p>
      <p className="text-center text-sm text-muted-foreground">
        Make an account, or log in if you have one. You'll join as soon as you're in.
      </p>
      <a href="/register" className={buttonVariants()}>
        Sign up
      </a>
      <a href="/login" className={buttonVariants({ variant: 'outline' })}>
        I have an account: log in
      </a>
    </div>
  );
}
