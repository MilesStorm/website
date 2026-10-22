import { useEffect, useState } from 'react';
import { Monitor, Moon, Sun } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { useMenu } from '@/lib/menu';
import { THEME_KEY, getTheme, setTheme, type Theme } from '@/lib/theme';
import { island } from '@/lib/island';

/** Light, dark or the system's theme (shadcn's Astro mode toggle). */
export const ModeToggle = island('ModeToggle', function ModeToggle() {
  const [theme, set] = useState<Theme>('system');
  const menu = useMenu();
  useEffect(() => {
    set(getTheme());
    // Chosen in another tab: the page follows (the layout's script), and so does the menu.
    const follow = (e: StorageEvent) => {
      if (e.key === THEME_KEY) set(getTheme());
    };
    window.addEventListener('storage', follow);
    return () => window.removeEventListener('storage', follow);
  }, []);
  const choose = (value: string) => {
    const next = value as Theme;
    setTheme(next);
    set(next);
  };
  return (
    <DropdownMenu {...menu.root}>
      <DropdownMenuTrigger asChild {...menu.trigger}>
        <Button variant="ghost" size="icon" aria-label="Theme" data-faro-user-action-name="theme">
          <Sun className="scale-100 rotate-0 transition-all dark:scale-0 dark:-rotate-90" />
          <Moon className="absolute scale-0 rotate-90 transition-all dark:scale-100 dark:rotate-0" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuRadioGroup value={theme} onValueChange={choose}>
          <DropdownMenuRadioItem value="light">
            <Sun /> Light
          </DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="dark">
            <Moon /> Dark
          </DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="system">
            <Monitor /> System
          </DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
});
