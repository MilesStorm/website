import { Download } from 'lucide-react';
import { buttonVariants } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import { StatCard } from '@/components/ServerCards';
import { EXTENSION_XPI } from '@/lib/arcane';
import { cn } from '@/lib/utils';

// The dice reader's cards under the camera, shared by the reader (islands/ArcaneCamera.tsx) and
// its placeholder: the last roll and the browser extension. Free of the API client: the
// placeholder is rendered when the page is built. The camera's place is a fixed 16:9 frame (the
// camera's own shape is only known once it runs, and whatever it is, it is fitted inside this
// one), so the page keeps its size from the placeholder through the camera's every state.

/** The last roll the reader settled on, or a nudge to roll. */
export function RollCard({ roll }: { roll: string | null }) {
  return (
    <StatCard
      label="Last roll"
      live
      value={roll ?? <span className="text-base font-normal text-muted-foreground">Roll some dice</span>}
      footer={
        <p className="text-xs text-muted-foreground">
          Your latest roll's picture is kept for 10 minutes so you can flag it as wrong from the browser extension. It's
          only saved for training if you flag it or turn on sharing in your profile.
        </p>
      }
    />
  );
}

/** The Firefox extension, for Roll20. */
export function ExtensionCard() {
  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2>Playing on Roll20?</h2>
        </CardTitle>
        <CardDescription>The Firefox extension types each roll into your game's chat, where Roll20 shows the total.</CardDescription>
      </CardHeader>
      <CardContent>
        <a className={buttonVariants({ variant: 'outline' })} href={EXTENSION_XPI}>
          <Download aria-hidden="true" />
          Install the extension
        </a>
      </CardContent>
      <CardFooter>
        <p className="text-xs text-muted-foreground">
          Firefox asks you to confirm. If it only downloads the file, open it from the downloads list. It updates itself
          after that.
        </p>
      </CardFooter>
    </Card>
  );
}

/** The page: the camera on the left, as large as its column allows, and the cards in a column of
 * 22rem on its right; stacked below `lg`. */
export const ARCANE_GRID = 'grid gap-6 lg:grid-cols-[minmax(0,1fr)_22rem] lg:items-start';

/** The camera's column: the frame, and what comes up under it (the debug readout). */
export const CAMERA_COLUMN = 'flex min-w-0 flex-col gap-4';

/** The cards beside the camera: one under the other, or two side by side where there is room
 * for it but not for the columns (`md` up to `lg`). */
export const SIDE_CARDS = 'grid gap-4 md:grid-cols-2 lg:grid-cols-1';

/** The camera's frame: 16:9, and tall enough on a phone for the longest message it holds (the
 * camera refused: an icon, three or four lines and a button). */
export const CAMERA_FRAME = 'relative aspect-video min-h-72 w-full overflow-hidden rounded-2xl bg-muted ring-1 ring-foreground/10 sm:min-h-0';

export function ArcaneSkeleton() {
  return (
    <div role="status" aria-label="Starting the camera" aria-busy="true" className={ARCANE_GRID}>
      <div className={CAMERA_COLUMN}>
        <div className={cn(CAMERA_FRAME, 'animate-pulse')} />
      </div>
      <div className={SIDE_CARDS}>
        <RollCard roll={null} />
        <ExtensionCard />
      </div>
    </div>
  );
}
