// A form's or a page's error (not one field's): announced, near the button. The text is the
// foreground colour, with the icon and border in the destructive one: shadcn's destructive text
// on a card is under WCAG's contrast minimum.
import { CircleAlert } from 'lucide-react';
import { Alert, AlertDescription } from '@/components/ui/alert';

export function FormAlert({ message }: { message: string | null | undefined }) {
  if (!message) return null;
  return (
    <Alert className="border-destructive/50">
      <CircleAlert aria-hidden="true" className="text-destructive" />
      <AlertDescription className="text-foreground">{message}</AlertDescription>
    </Alert>
  );
}
