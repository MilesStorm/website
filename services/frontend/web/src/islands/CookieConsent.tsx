import { useEffect, useRef, useState } from 'react';
import { Cookie } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { getConsent, setConsent } from '@/lib/theme';
import { island } from '@/lib/island';

/** The room the open notice takes at the bottom of the window (global.css pads the page by it). */
export const CONSENT_HEIGHT = '--consent-h';
/** The notice's distance from the window's bottom edge (`bottom-4`). */
const GAP_PX = 16;

/** Says which cookies the site uses, until accepted or dismissed (`ms.cookie-consent`). While it is
 * open, the page ends that much lower, so nothing under the notice is out of reach. */
export const CookieConsent = island('CookieConsent', function CookieConsent() {
  const [open, setOpen] = useState(false);
  const notice = useRef<HTMLDivElement>(null);
  useEffect(() => setOpen(getConsent() === null), []);
  useEffect(() => {
    const root = document.documentElement;
    if (!open || !notice.current) return;
    const measure = new ResizeObserver(([entry]) => {
      const height = entry.borderBoxSize?.[0]?.blockSize ?? entry.target.getBoundingClientRect().height;
      root.style.setProperty(CONSENT_HEIGHT, `${Math.ceil(height) + GAP_PX}px`);
    });
    measure.observe(notice.current);
    return () => {
      measure.disconnect();
      root.style.removeProperty(CONSENT_HEIGHT);
    };
  }, [open]);
  if (!open) return null;
  const close = (value: 'accepted' | 'dismissed') => {
    setConsent(value);
    setOpen(false);
  };
  return (
    <Card
      ref={notice}
      role="region"
      aria-label="Cookies"
      className="fixed right-4 bottom-4 left-4 z-50 max-w-xl shadow-lg"
    >
      <CardContent className="flex flex-col gap-4 sm:flex-row sm:items-center">
        <Cookie className="size-6 shrink-0" aria-hidden="true" />
        <div className="flex-1">
          <p className="font-semibold">Cookies</p>
          <p className="text-sm text-muted-foreground">
            This site uses functional cookies required for login and session management.
          </p>
        </div>
        <div className="flex gap-2">
          <Button variant="ghost" size="sm" onClick={() => close('dismissed')} data-faro-user-action-name="cookies dismiss">
            Dismiss
          </Button>
          <Button size="sm" onClick={() => close('accepted')} data-faro-user-action-name="cookies accept">
            Accept
          </Button>
        </div>
      </CardContent>
    </Card>
  );
});
