import { useEffect, useState } from 'react';
import { Cookie } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { getConsent, setConsent } from '@/lib/theme';

/** Says which cookies the site uses, until accepted or dismissed (`ms.cookie-consent`). */
export function CookieConsent() {
  const [open, setOpen] = useState(false);
  useEffect(() => setOpen(getConsent() === null), []);
  if (!open) return null;
  const close = (value: 'accepted' | 'dismissed') => {
    setConsent(value);
    setOpen(false);
  };
  return (
    <Card role="region" aria-label="Cookies" className="fixed right-4 bottom-4 left-4 z-50 max-w-xl shadow-lg">
      <CardContent className="flex flex-col gap-4 sm:flex-row sm:items-center">
        <Cookie className="size-6 shrink-0" aria-hidden="true" />
        <div className="flex-1">
          <p className="font-semibold">Cookies</p>
          <p className="text-sm text-muted-foreground">
            This site uses functional cookies required for login and session management.
          </p>
        </div>
        <div className="flex gap-2">
          <Button variant="ghost" size="sm" onClick={() => close('dismissed')}>
            Dismiss
          </Button>
          <Button size="sm" onClick={() => close('accepted')}>
            Accept
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
