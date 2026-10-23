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
import { bffUrl } from '@/api/client';
import { logOut, onViewerChange, readBootstrap, type Viewer } from '@/api/session';
import { useMenu } from '@/lib/menu';
import defaultPicture from '@/assets/default-profile.png';
import { island } from '@/lib/island';

/** The logged-in user's menu: their name and picture, the profile, logging out. */
export const UserMenu = island('UserMenu', function UserMenu() {
  const [viewer, setViewer] = useState<Viewer | null>(null);
  const menu = useMenu();
  useEffect(() => {
    void readBootstrap().then((b) => setViewer(b.viewer));
    return onViewerChange(setViewer);
  }, []);
  if (!viewer) return <span className="block size-9" aria-hidden="true" />;

  return (
    <DropdownMenu {...menu.root}>
      <DropdownMenuTrigger asChild {...menu.trigger}>
        <Button
          variant="ghost"
          size="icon"
          className="rounded-full"
          aria-label={`Account: ${viewer.shown_name}`}
          data-faro-user-action-name="account menu"
        >
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
});
