import { useEffect, useState } from 'react';
import { LogOut, User } from 'lucide-react';
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { bffUrl, client } from '@/api/client';
import { readBootstrap, type Viewer } from '@/api/session';
import defaultPicture from '@/assets/default-profile.png';

/** The logged-in user's menu: their name and picture, the profile, logging out. */
export function UserMenu() {
  const [viewer, setViewer] = useState<Viewer | null>(null);
  useEffect(() => {
    void readBootstrap().then((b) => setViewer(b.viewer));
  }, []);
  if (!viewer) return <span className="block size-9" aria-hidden="true" />;

  const logOut = async () => {
    await client.DELETE('/api/session');
    // A new page, so everything on it is the logged-out one.
    location.assign(bffUrl('/'));
  };
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" className="rounded-full" aria-label={`Account: ${viewer.shown_name}`}>
          <Avatar>
            <AvatarImage src={viewer.picture_url ? bffUrl(viewer.picture_url) : defaultPicture.src} alt="" />
            <AvatarFallback>{viewer.shown_name.slice(0, 1).toUpperCase()}</AvatarFallback>
          </Avatar>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-56">
        <DropdownMenuLabel className="truncate">{viewer.shown_name}</DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem asChild>
          <a href="/profile">
            <User /> Profile
          </a>
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => void logOut()}>
          <LogOut /> Log out
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
