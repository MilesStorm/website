// Logging in with GitHub or Google: plain links to the server, which keeps `next` for when the
// provider sends the browser back.
import { buttonVariants } from '@/components/ui/button';
import { oauthStart } from '@/lib/login';
import type { PagePath } from '@/lib/pages';

export function OAuthButtons({ next, verb }: { next: PagePath | null; verb: 'Log in' | 'Sign up' }) {
  const button = buttonVariants({ variant: 'outline', className: 'w-full' });
  return (
    <div className="flex flex-col gap-3">
      <a href={oauthStart('github', next)} className={button}>
        <GitHubIcon /> {verb} with GitHub
      </a>
      <a href={oauthStart('google', next)} className={button}>
        <GoogleIcon /> {verb} with Google
      </a>
    </div>
  );
}

function GitHubIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" fill="currentColor">
      <path d="M12 2a10 10 0 0 0-3.16 19.49c.5.08.66-.23.66-.5v-1.69c-2.77.6-3.36-1.34-3.36-1.34-.46-1.16-1.11-1.47-1.11-1.47-.91-.62.07-.6.07-.6 1 .07 1.53 1.03 1.53 1.03.87 1.52 2.34 1.07 2.91.83.09-.65.35-1.09.63-1.34-2.22-.25-4.55-1.11-4.55-4.92 0-1.11.38-2 1.03-2.71-.1-.25-.45-1.29.1-2.64 0 0 .84-.27 2.75 1.02a9.58 9.58 0 0 1 5 0c1.91-1.29 2.75-1.02 2.75-1.02.55 1.35.2 2.39.1 2.64.65.71 1.03 1.6 1.03 2.71 0 3.82-2.34 4.66-4.57 4.91.36.31.69.92.69 1.85v2.75c0 .27.16.59.67.5A10 10 0 0 0 12 2Z" />
    </svg>
  );
}

function GoogleIcon() {
  return (
    <svg viewBox="0 0 512 512" aria-hidden="true">
      <path d="M153 292c30 82 118 95 171 60h62v48A192 192 0 0190 341" fill="#34a853" />
      <path d="m386 400a140 175 0 0053-179H260v74h102q-7 37-38 57" fill="#4285f4" />
      <path d="m90 341a208 200 0 010-171l63 49q-12 37 0 73" fill="#fbbc02" />
      <path d="m153 219c22-69 116-109 179-50l55-54c-78-75-230-72-297 55" fill="#ea4335" />
    </svg>
  );
}
