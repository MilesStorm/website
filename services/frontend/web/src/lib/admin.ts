// The admin panel's rules that aren't layout: invite statuses, the pager's buttons, the new-invite
// form's checks.
import type { Schemas } from '@/api/client';

/** Rows per page of the users and roles listings (the server takes 1 to 100). */
export const PAGE_SIZE = 25;

/** New invite links give this role unless another is chosen: the Arcane dice test group. There is
 * no fallback to another role, which could quietly make a link for an admin role. */
export const DEFAULT_INVITE_ROLE = 'arcane_user';

/** An invite's status as the list shows it. Turned off wins, even when also used up. */
export function inviteStatus(invite: Schemas['AdminInvite']): 'Active' | 'Turned off' | 'Used up' | 'Expired' {
  if (invite.revoked) return 'Turned off';
  if (invite.live) return 'Active';
  if (invite.max_uses !== null && invite.uses >= invite.max_uses) return 'Used up';
  return 'Expired';
}

/** "2 / 5", or "2" without a limit. */
export function inviteUses(invite: Schemas['AdminInvite']): string {
  return invite.max_uses === null ? String(invite.uses) : `${invite.uses} / ${invite.max_uses}`;
}

/** The date of an RFC 3339 timestamp (UTC), as the list shows it. */
export function utcDate(timestamp: string): string {
  return timestamp.slice(0, 10);
}

/** The pager's buttons: the first two, the last two and the current one ± 2 (page numbers from
 * 0), with `null` for each gap. Nothing when everything fits on one page. */
export function pageButtons(page: number, total: number, limit: number = PAGE_SIZE): (number | null)[] {
  const pages = Math.ceil(total / limit);
  if (pages <= 1) return [];
  const out: (number | null)[] = [];
  for (let i = 0; i < pages; i++) {
    if (i < 2 || i >= pages - 2 || Math.abs(i - page) <= 2) out.push(i);
    else if (out.at(-1) !== null) out.push(null);
  }
  return out;
}

/** The new-invite form, read: the request, or what is wrong with it. */
export function newInvite(form: { roleId: number; days: string; maxUses: string; note: string }):
  | { ok: true; invite: Schemas['NewInvite'] }
  | { ok: false; message: string } {
  const days = Number(form.days.trim());
  if (!form.days.trim() || !Number.isInteger(days)) return { ok: false, message: 'Days must be a number.' };
  const uses = form.maxUses.trim();
  const maxUses = uses === '' ? null : Number(uses);
  if (maxUses !== null && !Number.isInteger(maxUses)) {
    return { ok: false, message: '“How many people” must be a number, or left empty for no limit.' };
  }
  if (!form.roleId) return { ok: false, message: 'Choose which role the link gives.' };
  const note = form.note.trim() || null;
  return { ok: true, invite: { role_id: form.roleId, days, max_uses: maxUses, note } };
}
