import { useEffect, useState, type ComponentProps, type ReactNode } from 'react';
import { Monitor, Moon, Sun } from 'lucide-react';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group';
import { Setting, SettingList } from '@/components/Setting';
import { APPEARANCE_INTRO, MOTION_INTRO, THEME_INTRO } from '@/lib/copy';
import { cn } from '@/lib/utils';
import { MOTION_KEY, THEME_KEY, getMotion, getTheme, setMotion, switchTheme, type Motion, type Theme } from '@/lib/theme';

// The profile's Appearance card: the theme and the motion as segmented controls, each showing
// what is saved in this browser and following the other tabs.

/** A stored choice, read when the control first shows and again when another tab changes it. */
function useStored<T>(key: string, read: () => T) {
  const [value, set] = useState(read);
  useEffect(() => {
    set(read());
    const follow = (e: StorageEvent) => {
      if (e.key === key) set(read());
    };
    window.addEventListener('storage', follow);
    return () => window.removeEventListener('storage', follow);
  }, [key, read]);
  return [value, set] as const;
}

/** One choice out of a few, as shadcn's segmented control. The group's own change is ignored
 * (clicking the chosen one would clear it); each item's click is the choice. */
function Segmented({ label, value, children }: { label: string; value: string; children: ReactNode }) {
  return (
    <ToggleGroup
      type="single"
      aria-label={label}
      value={value}
      spacing={0.5}
      className="w-full bg-muted p-[3px] sm:w-fit"
    >
      {children}
    </ToggleGroup>
  );
}

function Choice({ className, ...props }: ComponentProps<typeof ToggleGroupItem>) {
  return (
    <ToggleGroupItem
      className={cn(
        'h-7 flex-1 text-foreground/60 sm:flex-none hover:bg-transparent hover:text-foreground data-[state=on]:bg-background data-[state=on]:text-foreground dark:text-muted-foreground dark:data-[state=on]:bg-input/50',
        className,
      )}
      {...props}
    />
  );
}

const THEMES: [Theme, string, typeof Sun][] = [
  ['light', 'Light', Sun],
  ['dark', 'Dark', Moon],
  ['system', 'System', Monitor],
];
const MOTIONS: [Motion, string][] = [
  ['system', 'System'],
  ['reduced', 'Reduced'],
  ['full', 'Full'],
];

export function AppearanceCard() {
  const [theme, setThemeState] = useStored(THEME_KEY, getTheme);
  const [motion, setMotionState] = useStored(MOTION_KEY, getMotion);
  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2>Appearance</h2>
        </CardTitle>
        <CardDescription>{APPEARANCE_INTRO}</CardDescription>
      </CardHeader>
      <CardContent>
        <SettingList>
          <Setting wide title="Theme" description={THEME_INTRO}>
            <Segmented label="Theme" value={theme}>
              {THEMES.map(([value, name, Icon]) => (
                <Choice
                  key={value}
                  value={value}
                  data-faro-user-action-name="theme"
                  onClick={(e) => {
                    if (value === theme) return;
                    switchTheme(value, e.currentTarget);
                    setThemeState(value);
                  }}
                >
                  <Icon /> {name}
                </Choice>
              ))}
            </Segmented>
          </Setting>
          <Setting wide title="Motion" description={MOTION_INTRO}>
            <Segmented label="Motion" value={motion}>
              {MOTIONS.map(([value, name]) => (
                <Choice
                  key={value}
                  value={value}
                  data-faro-user-action-name="motion"
                  onClick={() => {
                    if (value === motion) return;
                    setMotion(value);
                    setMotionState(value);
                  }}
                >
                  {name}
                </Choice>
              ))}
            </Segmented>
          </Setting>
        </SettingList>
      </CardContent>
    </Card>
  );
}
