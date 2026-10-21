import type { ReactNode } from 'react';
import { Skeleton } from '@/components/ui/skeleton';
import { Field, FieldDescription, FieldGroup, FieldLabel, FieldSeparator } from '@/components/ui/field';
import { MIN_PASSWORD } from '@/lib/password';

/** A form's place while its island loads. */
export function FormSkeleton() {
  return (
    <div className="flex flex-col gap-4" aria-busy="true">
      <Skeleton className="h-9 w-full" />
      <Skeleton className="h-9 w-full" />
      <Skeleton className="h-9 w-full" />
    </div>
  );
}

// The logging-in forms' places: the form's own layout, text and labels, with a pulse where each
// input and button goes, so nothing moves when the form arrives (e2e/auth.spec.ts measures it).

/** An input's or a button's place (both are h-9). */
function Control() {
  return <Skeleton className="h-9 w-full" />;
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
    <div aria-busy="true">
      <FieldGroup>{children}</FieldGroup>
    </div>
  );
}

function OAuth() {
  return (
    <>
      <Field>
        <Control />
        <Control />
      </Field>
      <FieldSeparator className="*:data-[slot=field-separator-content]:bg-card">Or continue with</FieldSeparator>
    </>
  );
}

function Submit({ note }: { note?: string }) {
  return (
    <Field>
      <Control />
      {note && <FieldDescription className="text-center">{note}</FieldDescription>}
    </Field>
  );
}

export function LoginSkeleton() {
  return (
    <Frame>
      <OAuth />
      <Labelled label="Username" />
      <Labelled label="Password" action="Forgot your password?" />
      <Submit note="Don't have an account? Sign up" />
    </Frame>
  );
}

export function RegisterSkeleton() {
  return (
    <Frame>
      <OAuth />
      <Labelled label="Email" />
      <Labelled label="Username" />
      <Labelled label="Password" description={`At least ${MIN_PASSWORD} characters. We'll email you a link to confirm your address.`} />
      <Submit note="Already have an account? Log in" />
    </Frame>
  );
}

export function ForgotSkeleton() {
  return (
    <Frame>
      <Labelled label="Username or email" />
      <Submit note="Log in with GitHub or Google? Then you have no password here: use that button on the log in page." />
    </Frame>
  );
}

export function ResetSkeleton() {
  return (
    <Frame>
      <Labelled label="New password" description={`At least ${MIN_PASSWORD} characters.`} />
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
