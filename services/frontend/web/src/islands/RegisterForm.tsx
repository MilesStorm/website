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
import { attempt } from '@/lib/errors';
import { landAfterLogin, nextFrom } from '@/lib/login';
import { HAVE_ACCOUNT, MIN_PASSWORD, REGISTER_PASSWORD_HINT } from '@/lib/copy';

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
    <div className="grid gap-6">
      <form onSubmit={submit} noValidate>
        <FieldGroup>
          <TextField form={form} name="email" label="Email" type="email" autoComplete="email" />
          <TextField form={form} name="username" label="Username" autoComplete="username" />
          <TextField
            form={form}
            name="password"
            label="Password"
            type="password"
            autoComplete="new-password"
            description={REGISTER_PASSWORD_HINT}
          />
          <FormAlert message={error} />
          <Field>
            <Button type="submit" disabled={form.formState.isSubmitting}>
              Sign up with email
            </Button>
          </Field>
        </FieldGroup>
      </form>
      <FieldSeparator className="*:data-[slot=field-separator-content]:bg-card">Or continue with</FieldSeparator>
      <OAuthButtons next={next} verb="Sign up" />
      <FieldDescription className="px-6 text-center">
        {HAVE_ACCOUNT.text} <a href={login}>{HAVE_ACCOUNT.link}</a>
      </FieldDescription>
    </div>
  );
}
