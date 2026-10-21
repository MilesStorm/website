import { Download } from 'lucide-react';
import { buttonVariants } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import { StatCard } from '@/components/ServerCards';
import { EXTENSION_XPI } from '@/lib/arcane';

// The dice reader's cards under the camera, shared by the reader (islands/ArcaneCamera.tsx) and
// its placeholder: the last roll and the browser extension. Free of the API client: the
// placeholder is rendered when the page is built. The camera's place is a 16:9 pulse; the
// camera's own shape is only known once it runs.

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

/** The cards side by side under the camera. */
export const UNDER_CAMERA = 'grid gap-4 md:grid-cols-2';

export function ArcaneSkeleton() {
  return (
    <div role="status" aria-label="Starting the camera" aria-busy="true" className="flex flex-col gap-6">
      <div className="aspect-video w-full animate-pulse rounded-2xl bg-muted" />
      <div className={UNDER_CAMERA}>
        <RollCard roll={null} />
        <ExtensionCard />
      </div>
    </div>
  );
}
