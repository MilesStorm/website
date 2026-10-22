import { useState } from 'react';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { z } from 'zod';
import { CircleCheck } from 'lucide-react';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Field, FieldDescription, FieldGroup } from '@/components/ui/field';
import { FormAlert } from '@/components/FormAlert';
import { TextField } from '@/components/TextField';
import { client } from '@/api/client';
import { FORGOT_INTRO, NO_PASSWORD } from '@/lib/copy';
import { attempt } from '@/lib/errors';
import { island } from '@/lib/island';

const schema = z.object({ login: z.string().trim().min(1, 'Enter your username or email.') });

/** Asking for a password reset link. The answer is the same whether the account exists or not. */
export const ForgotForm = island('ForgotForm', function ForgotForm() {
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
    <form onSubmit={submit} noValidate data-faro-user-action-name="forgot password">
      <FieldGroup>
        <FieldDescription className="text-center">{FORGOT_INTRO}</FieldDescription>
        <TextField form={form} name="login" label="Username or email" autoComplete="username" />
        <FormAlert message={error} />
        <Field>
          <Button type="submit" disabled={form.formState.isSubmitting}>
            Email me a link
          </Button>
          <FieldDescription className="text-center">
            {NO_PASSWORD.text} <a href="/login">{NO_PASSWORD.link}</a>
            {NO_PASSWORD.end}
          </FieldDescription>
        </Field>
      </FieldGroup>
    </form>
  );
});
