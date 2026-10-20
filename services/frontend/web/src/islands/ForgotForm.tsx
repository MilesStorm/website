import { useState } from 'react';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { z } from 'zod';
import { CircleCheck } from 'lucide-react';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { FormAlert } from '@/components/FormAlert';
import { TextField } from '@/components/TextField';
import { client } from '@/api/client';
import { attempt } from '@/lib/errors';

const schema = z.object({ login: z.string().trim().min(1, 'Enter your username or email.') });

/** Asking for a password reset link. The answer is the same whether the account exists or not. */
export function ForgotForm() {
  const [sent, setSent] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const form = useForm({ resolver: zodResolver(schema), defaultValues: { login: '' } });

  const submit = form.handleSubmit(async (body) => {
    setError(null);
    const done = await attempt(client.POST('/api/password/forgot', { body }));
    if (done.ok) setSent(true);
    else setError(done.message);
  });

  if (sent) {
    return (
      <Alert>
        <CircleCheck aria-hidden="true" />
        <AlertDescription>
          If that account has an email address, we've sent it a link to choose a new password. It works for 1 hour.
          Check your spam folder too.
        </AlertDescription>
      </Alert>
    );
  }
  return (
    <form onSubmit={submit} noValidate className="flex flex-col gap-6">
      <p className="text-sm text-muted-foreground">
        Enter your username or email, and we'll email you a link to choose a new password.
      </p>
      <TextField form={form} name="login" label="Username or email" autoComplete="username" />
      <FormAlert message={error} />
      <Button type="submit" disabled={form.formState.isSubmitting}>
        Email me a link
      </Button>
      <p className="text-xs text-muted-foreground">
        Log in with GitHub or Google? Then you have no password here: use that button on the log in page.
      </p>
    </form>
  );
}
