import type { ReactNode } from 'react';
import { Skeleton } from '@/components/ui/skeleton';
import { Field, FieldDescription, FieldGroup, FieldLabel, FieldSeparator } from '@/components/ui/field';
import {
  DELETE_LINK,
  FORGOT_INTRO,
  FORGOT_LINK,
  HAVE_ACCOUNT,
  NEW_PASSWORD_HINT,
  NO_ACCOUNT,
  NO_PASSWORD,
  REGISTER_PASSWORD_HINT,
  type Note,
} from '@/lib/copy';

// The logging-in forms' places: the form's own layout, text and labels, with a pulse where each
// input and button goes, so nothing moves when the form arrives (e2e/auth.spec.ts measures it).

/** An input's or a button's place (both are h-9). */
function Control() {
  return <Skeleton className="h-9 w-full rounded-4xl" />;
}

function Labelled({ label, action, description }: { label: string; action?: string; description?: string }) {
  return (
    <Field>
      {action ? (
        <div className="flex items-center">
          <FieldLabel>{label}</FieldLabel>
          <span className="ml-auto text-sm">{action}</span>
        </div>
      ) : (
        <FieldLabel>{label}</FieldLabel>
      )}
      <Control />
      {description && <FieldDescription>{description}</FieldDescription>}
    </Field>
  );
}

function Frame({ children }: { children: ReactNode }) {
  return (
    <div role="status" aria-label="Loading" aria-busy="true">
      <FieldGroup>{children}</FieldGroup>
    </div>
  );
}

/** The password form, then the providers' buttons and a line under them (LoginForm, RegisterForm). */
function WithProviders({ children, note }: { children: ReactNode; note: Note }) {
  return (
    <div role="status" aria-label="Loading" aria-busy="true" className="grid gap-6">
      <div>
        <FieldGroup>
          {children}
          <Field>
            <Control />
          </Field>
        </FieldGroup>
      </div>
      <FieldSeparator className="*:data-[slot=field-separator-content]:bg-card">Or continue with</FieldSeparator>
      <Field>
        <Control />
        <Control />
      </Field>
      <FieldDescription className="px-6 text-center">
        {note.text} {note.link}
      </FieldDescription>
    </div>
  );
}

function Submit({ note }: { note?: Note }) {
  return (
    <Field>
      <Control />
      {note && (
        <FieldDescription className="text-center">
          {note.text} {note.link}
          {note.end}
        </FieldDescription>
      )}
    </Field>
  );
}

export function LoginSkeleton() {
  return (
    <WithProviders note={NO_ACCOUNT}>
      <Labelled label="Username" />
      <Labelled label="Password" action={FORGOT_LINK} />
    </WithProviders>
  );
}

export function RegisterSkeleton() {
  return (
    <WithProviders note={HAVE_ACCOUNT}>
      <Labelled label="Email" />
      <Labelled label="Username" />
      <Labelled label="Password" description={REGISTER_PASSWORD_HINT} />
    </WithProviders>
  );
}

export function ForgotSkeleton() {
  return (
    <Frame>
      <FieldDescription className="text-center">{FORGOT_INTRO}</FieldDescription>
      <Labelled label="Username or email" />
      <Submit note={NO_PASSWORD} />
    </Frame>
  );
}

export function ResetSkeleton() {
  return (
    <Frame>
      <Labelled label="New password" description={NEW_PASSWORD_HINT} />
      <Labelled label="Type it again" />
      <Submit />
    </Frame>
  );
}

/** Opening an emailed link: the place of what most often comes back (a message and a button). */
export function VerifySkeleton() {
  return (
    <div className="flex flex-col gap-4" role="status" aria-label="Confirming">
      <Skeleton className="h-17 w-full" />
      <Control />
    </div>
  );
}

/** Opening an invite: the place of the logged-out answer (a few lines and two buttons). */
export function InviteSkeleton() {
  return (
    <div className="flex flex-col gap-4" role="status" aria-label="Opening the invite">
      <Skeleton className="h-24 w-full" />
      <Control />
      <Control />
    </div>
  );
}

/** Opening a deletion link: the place of the usual answer (whose account, and two buttons). */
export function DeleteLinkSkeleton() {
  return (
    <div className="flex flex-col gap-4" role="status" aria-label="Checking the link">
      <p>
        {DELETE_LINK.before}{' '}
        <span className="animate-pulse rounded-md bg-muted font-semibold text-transparent">username</span>
        {DELETE_LINK.after}
      </p>
      <div className="flex flex-col gap-2 sm:flex-row-reverse">
        <Skeleton className="h-9 w-full rounded-4xl sm:flex-1" />
        <Skeleton className="h-9 w-full rounded-4xl sm:flex-1" />
      </div>
    </div>
  );
}
