// One labelled input of a react-hook-form form: its error is tied to it (`aria-describedby`)
// and announced (FieldError is an alert).
import { useId } from 'react';
import type { FieldValues, Path, UseFormReturn } from 'react-hook-form';
import { Field, FieldDescription, FieldError, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';

interface Props<T extends FieldValues> {
  form: UseFormReturn<T>;
  name: Path<T>;
  label: string;
  type?: 'text' | 'email' | 'password';
  autoComplete: string;
  description?: string;
}

export function TextField<T extends FieldValues>({ form, name, label, type = 'text', autoComplete, description }: Props<T>) {
  const id = useId();
  const { error } = form.getFieldState(name, form.formState);
  const described = [description && `${id}-description`, error && `${id}-error`].filter(Boolean).join(' ');
  return (
    <Field data-invalid={!!error || undefined}>
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Input
        id={id}
        type={type}
        autoComplete={autoComplete}
        aria-invalid={!!error || undefined}
        aria-describedby={described || undefined}
        {...form.register(name)}
      />
      {description && <FieldDescription id={`${id}-description`}>{description}</FieldDescription>}
      <FieldError id={`${id}-error`} errors={[error]} />
    </Field>
  );
}
