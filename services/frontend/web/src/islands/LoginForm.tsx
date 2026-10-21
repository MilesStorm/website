import { useState } from 'react';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { z } from 'zod';
import { Button } from '@/components/ui/button';
import { Field, FieldDescription, FieldGroup, FieldSeparator } from '@/components/ui/field';
import { FormAlert } from '@/components/FormAlert';
import { OAuthButtons } from '@/components/OAuthButtons';
import { TextField } from '@/components/TextField';
import { client } from '@/api/client';
import { attempt, oauthError } from '@/lib/errors';
import { landAfterLogin, nextFrom } from '@/lib/login';

const schema = z.object({
  username: z.string().trim().min(1, 'Enter your username.'),
  password: z.string().min(1, 'Enter your password.'),
});

/** Logging in: GitHub, Google or a password, then on to `?next=` (or a waiting invite). An OAuth
 * login that failed comes back here with `?error=`. */
export function LoginForm() {
  const [search] = useState(() => location.search);
  const next = nextFrom(search);
  const [error, setError] = useState(() => oauthError(new URLSearchParams(search).get('error')));
  const form = useForm({ resolver: zodResolver(schema), defaultValues: { username: '', password: '' } });

  const submit = form.handleSubmit(async (body) => {
    setError(null);
    const done = await attempt(client.POST('/api/session/login', { body }));
    if (!done.ok) return setError(done.message);
    await landAfterLogin(next);
  });

  const withNext = (path: string) => (next ? `${path}?next=${encodeURIComponent(next)}` : path);
  return (
    <form onSubmit={submit} noValidate>
      <FieldGroup>
        <OAuthButtons next={next} verb="Log in" />
        <FieldSeparator className="*:data-[slot=field-separator-content]:bg-card">Or continue with</FieldSeparator>
        <TextField form={form} name="username" label="Username" autoComplete="username" />
        <TextField
          form={form}
          name="password"
          label="Password"
          type="password"
          autoComplete="current-password"
          action={
            <a href="/forgot-password" className="text-sm underline-offset-4 hover:underline">
              Forgot your password?
            </a>
          }
        />
        <FormAlert message={error} />
        <Field>
          <Button type="submit" disabled={form.formState.isSubmitting}>
            Log in
          </Button>
          <FieldDescription className="text-center">
            Don&apos;t have an account? <a href={withNext('/register')}>Sign up</a>
          </FieldDescription>
        </Field>
      </FieldGroup>
    </form>
  );
}
