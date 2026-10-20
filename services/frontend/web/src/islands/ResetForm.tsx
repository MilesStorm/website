import { useState } from 'react';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { z } from 'zod';
import { CircleCheck } from 'lucide-react';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button, buttonVariants } from '@/components/ui/button';
import { FieldGroup } from '@/components/ui/field';
import { FormAlert } from '@/components/FormAlert';
import { TextField } from '@/components/TextField';
import { client } from '@/api/client';
import { applySession } from '@/api/session';
import { attempt } from '@/lib/errors';
import { MISSING_CODE, linkCode } from '@/lib/login';
import { MIN_PASSWORD } from './RegisterForm';

const schema = z
  .object({
    password: z.string().min(MIN_PASSWORD, `Use at least ${MIN_PASSWORD} characters.`),
    again: z.string(),
  })
  .refine((v) => v.password === v.again, { path: ['again'], message: 'The two passwords are different.' });

/** Choosing a new password from an emailed link (`/reset-password#<code>`). It logs the account
 * out everywhere, this browser included when it was that account's. */
export function ResetForm() {
  const [code] = useState(() => linkCode());
  const [done, setDone] = useState<string | null>(null);
  const [error, setError] = useState<{ message: string; expired: boolean } | null>(null);
  const form = useForm({ resolver: zodResolver(schema), defaultValues: { password: '', again: '' } });

  const submit = form.handleSubmit(async ({ password }) => {
    setError(null);
    const result = await attempt(client.POST('/api/password/reset', { body: { code, password } }));
    if (!result.ok) return setError({ message: result.message, expired: result.code === 'gone' });
    if (result.data.logged_out) applySession({ viewer: null });
    setDone(result.data.username);
  });

  if (!code) return <FormAlert message={MISSING_CODE} />;
  if (done !== null) {
    return (
      <div className="flex flex-col gap-4">
        <Alert>
          <CircleCheck aria-hidden="true" />
          <AlertDescription>
            <p>
              Your password has been changed, and you've been logged out everywhere. Log in as{' '}
              <span className="font-semibold text-foreground">{done}</span> with your new password.
            </p>
          </AlertDescription>
        </Alert>
        <a href="/login" className={buttonVariants()}>
          Log in
        </a>
      </div>
    );
  }
  return (
    <form onSubmit={submit} noValidate className="flex flex-col gap-6">
      <FieldGroup>
        <TextField
          form={form}
          name="password"
          label="New password"
          type="password"
          autoComplete="new-password"
          description={`At least ${MIN_PASSWORD} characters.`}
        />
        <TextField form={form} name="again" label="Type it again" type="password" autoComplete="new-password" />
      </FieldGroup>
      <FormAlert message={error?.message} />
      {error?.expired && (
        <a href="/forgot-password" className="text-center text-sm underline underline-offset-4">
          Get a new link
        </a>
      )}
      <Button type="submit" disabled={form.formState.isSubmitting}>
        Change password
      </Button>
    </form>
  );
}
