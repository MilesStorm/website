import { useEffect, useState } from 'react';
import { ChevronDown } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { useMenu } from '@/lib/menu';
import { MOTION_KEY, getMotion, setMotion, type Motion } from '@/lib/theme';
import { island } from '@/lib/island';

const LABELS: Record<Motion, string> = { system: 'System', reduced: 'Reduced', full: 'Full' };

/** Reduced, full or the system's motion: the start page's storm and the theme switch. */
export const MotionToggle = island('MotionToggle', function MotionToggle() {
  const [motion, set] = useState<Motion>('system');
  const menu = useMenu();
  useEffect(() => {
    set(getMotion());
    const follow = (e: StorageEvent) => {
      if (e.key === MOTION_KEY) set(getMotion());
    };
    window.addEventListener('storage', follow);
    return () => window.removeEventListener('storage', follow);
  }, []);
  const choose = (value: string) => {
    const next = value as Motion;
    setMotion(next);
    set(next);
  };
  return (
    <DropdownMenu {...menu.root}>
      <DropdownMenuTrigger asChild {...menu.trigger}>
        <Button
          variant="outline"
          size="sm"
          aria-label={`Motion: ${LABELS[motion]}`}
          data-faro-user-action-name="motion"
        >
          {LABELS[motion]} <ChevronDown />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuRadioGroup value={motion} onValueChange={choose}>
          {(Object.keys(LABELS) as Motion[]).map((value) => (
            <DropdownMenuRadioItem key={value} value={value}>
              {LABELS[value]}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
});
