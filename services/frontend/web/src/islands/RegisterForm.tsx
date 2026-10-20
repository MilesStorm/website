import { useState } from 'react';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { z } from 'zod';
import { Button } from '@/components/ui/button';
import { FieldGroup, FieldSeparator } from '@/components/ui/field';
import { FormAlert } from '@/components/FormAlert';
import { OAuthButtons } from '@/components/OAuthButtons';
import { TextField } from '@/components/TextField';
import { client } from '@/api/client';
import { attempt } from '@/lib/errors';
import { landAfterLogin, nextFrom } from '@/lib/login';

/** Auth's own minimum (services/auth); the server checks it too. */
export const MIN_PASSWORD = 8;

const schema = z.object({
  email: z.email('Enter an email address.'),
  username: z.string().trim().min(1, 'Choose a username.'),
  password: z.string().min(MIN_PASSWORD, `Use at least ${MIN_PASSWORD} characters.`),
});

/** Auth's answers for a name that is taken, by the field they are about. */
const TAKEN: Record<string, 'email' | 'username'> = {
  'Email already in use': 'email',
  'User already exists': 'username',
};

/** Signing up: GitHub, Google or a password; logged in at once, then on to `?next=`. */
export function RegisterForm() {
  const [next] = useState(() => nextFrom(location.search));
  const [error, setError] = useState<string | null>(null);
  const form = useForm({ resolver: zodResolver(schema), defaultValues: { email: '', username: '', password: '' } });

  const submit = form.handleSubmit(async (body) => {
    setError(null);
    const done = await attempt(client.POST('/api/session/register', { body }));
    if (done.ok) return landAfterLogin(next);
    const field = done.code === 'conflict' ? TAKEN[done.message] : undefined;
    if (field) form.setError(field, { message: `That ${field === 'email' ? 'email' : 'username'} is already in use.` });
    else setError(done.message);
  });

  const login = next ? `/login?next=${encodeURIComponent(next)}` : '/login';
  return (
    <div className="flex flex-col gap-6">
      <OAuthButtons next={next} verb="Sign up" />
      <FieldSeparator>or</FieldSeparator>
      <form onSubmit={submit} noValidate className="flex flex-col gap-6">
        <FieldGroup>
          <TextField form={form} name="email" label="Email" type="email" autoComplete="email" />
          <TextField form={form} name="username" label="Username" autoComplete="username" />
          <TextField
            form={form}
            name="password"
            label="Password"
            type="password"
            autoComplete="new-password"
            description={`At least ${MIN_PASSWORD} characters. We'll email you a link to confirm your address.`}
          />
        </FieldGroup>
        <FormAlert message={error} />
        <Button type="submit" disabled={form.formState.isSubmitting}>
          Sign up with email
        </Button>
      </form>
      <p className="text-center text-sm text-muted-foreground">
        Have an account?{' '}
        <a href={login} className="text-foreground underline underline-offset-4">
          Log in
        </a>
      </p>
    </div>
  );
}
