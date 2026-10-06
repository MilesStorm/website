use dioxus::prelude::*;

use api::{
    admin_assign_role_permission, admin_assign_user_role, admin_create_invite, admin_list_all_roles,
    admin_list_invites, admin_list_permissions, admin_list_roles, admin_list_users, admin_revoke_invite,
    admin_revoke_role_permission, admin_revoke_user_role, AdminInvite, AdminPermission, AdminRole, AdminUser,
};
use ui::data_dir::LoginStatus;

use crate::emails::server_message;
use crate::{LOGIN_STATUS, PERMISSIONS};

const PAGE_SIZE: u32 = 25;

#[component]
pub fn AdminPanel() -> Element {
    let has_perm = PERMISSIONS.read().contains_key("manage_permissions");

    match LOGIN_STATUS() {
        LoginStatus::LoggedOut => rsx! {
            div { class: "flex h-screen items-center justify-center",
                p { "Please log in to access the admin panel." }
            }
        },
        LoginStatus::LoggedIn(_) if !has_perm => rsx! {
            div { class: "flex h-screen items-center justify-center",
                p { "You do not have permission to access the admin panel." }
            }
        },
        LoginStatus::LoggedIn(_) => rsx! { AdminPanelInner {} },
    }
}

#[derive(Clone, PartialEq)]
enum Tab {
    Users,
    Roles,
    Invites,
}

#[component]
fn AdminPanelInner() -> Element {
    let mut tab = use_signal(|| Tab::Users);

    // all_roles and all_permissions are small lists used only for assignment dropdowns.
    let support = use_resource(move || async move {
        let r = admin_list_all_roles().await;
        let p = admin_list_permissions().await;
        (r, p)
    });

    let (all_roles, all_permissions, support_error) = match support.value()() {
        None => return rsx! {
            div { class: "flex justify-center p-20",
                span { class: "loading loading-spinner loading-lg" }
            }
        },
        Some((Ok(r), Ok(p))) => (r, p, None),
        Some((r, p)) => {
            let mut errs = Vec::new();
            if let Err(e) = &r { errs.push(format!("roles: {e}")); }
            if let Err(e) = &p { errs.push(format!("permissions: {e}")); }
            (r.unwrap_or_default(), p.unwrap_or_default(), Some(errs))
        }
    };

    rsx! {
        div { class: "container mx-auto mt-10 px-4",
            div { class: "bg-base-200 p-8 rounded-lg shadow-lg",
                h1 { class: "text-2xl font-bold mb-6", "Admin Panel" }

                if let Some(errs) = support_error {
                    div { class: "alert alert-error mb-4",
                        ul { class: "list-disc list-inside text-sm font-mono",
                            for e in errs { li { "{e}" } }
                        }
                    }
                }

                div { class: "tabs tabs-boxed mb-6",
                    button {
                        class: if tab() == Tab::Users { "tab tab-active" } else { "tab" },
                        onclick: move |_| tab.set(Tab::Users),
                        "Users"
                    }
                    button {
                        class: if tab() == Tab::Roles { "tab tab-active" } else { "tab" },
                        onclick: move |_| tab.set(Tab::Roles),
                        "Roles"
                    }
                    button {
                        class: if tab() == Tab::Invites { "tab tab-active" } else { "tab" },
                        onclick: move |_| tab.set(Tab::Invites),
                        "Invites"
                    }
                }

                match tab() {
                    Tab::Users => rsx! { UsersTab { all_roles } },
                    Tab::Roles => rsx! { RolesTab { all_permissions } },
                    Tab::Invites => rsx! { InvitesTab { all_roles } },
                }
            }
        }
    }
}

// ── Users tab ─────────────────────────────────────────────────────────────────

#[component]
fn UsersTab(all_roles: Vec<AdminRole>) -> Element {
    let mut search = use_signal(String::new);
    let mut page = use_signal(|| 0u32);
    let mut refresh = use_signal(|| 0u32);
    let mut load_error: Signal<Option<String>> = use_signal(|| None);

    let data = use_resource(move || {
        let s = search();
        let p = page();
        let _ = refresh();
        async move { admin_list_users(p, PAGE_SIZE, s).await }
    });

    let (users, total) = match data.value()() {
        None => {
            return rsx! {
                div { class: "flex justify-center p-12",
                    span { class: "loading loading-spinner loading-lg" }
                }
            }
        }
        Some(Ok(r)) => {
            load_error.set(None);
            (r.items, r.total)
        }
        Some(Err(e)) => {
            load_error.set(Some(e.to_string()));
            (vec![], 0i64)
        }
    };

    rsx! {
        div { class: "space-y-3",
            // Search + count row
            div { class: "flex items-center gap-3",
                input {
                    class: "input input-bordered input-sm w-full max-w-sm",
                    r#type: "text",
                    placeholder: "Search by username or email…",
                    value: "{search}",
                    oninput: move |e| {
                        search.set(e.value());
                        page.set(0);
                    },
                }
                span { class: "text-sm text-base-content/50 shrink-0", "{total} user(s)" }
            }

            if let Some(err) = load_error() {
                div { class: "alert alert-error text-sm font-mono", "{err}" }
            }

            div { class: "overflow-x-auto",
                table { class: "table w-full",
                    thead {
                        tr {
                            th { "Username" }
                            th { "Email" }
                            th { "Roles" }
                        }
                    }
                    tbody {
                        for user in users {
                            UserRow {
                                key: "{user.id}",
                                user: user.clone(),
                                all_roles: all_roles.clone(),
                                on_change: move |_| *refresh.write() += 1,
                            }
                        }
                    }
                }
            }

            Pagination {
                page: page(),
                total,
                limit: PAGE_SIZE,
                on_page: move |p| page.set(p),
            }
        }
    }
}

#[component]
fn UserRow(user: AdminUser, all_roles: Vec<AdminRole>, on_change: EventHandler<()>) -> Element {
    let unassigned: Vec<AdminRole> = all_roles
        .iter()
        .filter(|r| !user.roles.iter().any(|ur| ur.id == r.id))
        .cloned()
        .collect();

    let mut selected_role_id =
        use_signal(|| unassigned.first().map(|r| r.id).unwrap_or(0i32));

    rsx! {
        tr {
            td { class: "font-medium", "{user.username}" }
            td { class: "text-base-content/60", { user.email.as_deref().unwrap_or("—") } }
            td {
                div { class: "flex flex-wrap gap-1 items-center",
                    for role in user.roles.iter() {
                        {
                            let role_id = role.id;
                            let user_id = user.id;
                            let role_name = role.name.clone();
                            rsx! {
                                span { class: "badge badge-primary gap-1",
                                    "{role_name}"
                                    button {
                                        class: "btn btn-ghost btn-xs p-0 min-h-0 h-auto leading-none",
                                        onclick: move |_| {
                                            spawn(async move {
                                                let _ = admin_revoke_user_role(user_id, role_id).await;
                                                on_change.call(());
                                            });
                                        },
                                        "✕"
                                    }
                                }
                            }
                        }
                    }
                    if !unassigned.is_empty() {
                        div { class: "flex gap-1 items-center",
                            select {
                                class: "select select-xs select-bordered",
                                onchange: move |e: Event<FormData>| {
                                    if let Ok(id) = e.value().parse::<i32>() {
                                        selected_role_id.set(id);
                                    }
                                },
                                for role in &unassigned {
                                    option { value: "{role.id}", "{role.name}" }
                                }
                            }
                            button {
                                class: "btn btn-xs btn-success",
                                onclick: move |_| {
                                    let role_id = selected_role_id();
                                    let user_id = user.id;
                                    spawn(async move {
                                        let _ = admin_assign_user_role(user_id, role_id).await;
                                        on_change.call(());
                                    });
                                },
                                "+"
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── Roles tab ─────────────────────────────────────────────────────────────────

#[component]
fn RolesTab(all_permissions: Vec<AdminPermission>) -> Element {
    let mut search = use_signal(String::new);
    let mut page = use_signal(|| 0u32);
    let mut refresh = use_signal(|| 0u32);
    let mut load_error: Signal<Option<String>> = use_signal(|| None);

    let data = use_resource(move || {
        let s = search();
        let p = page();
        let _ = refresh();
        async move { admin_list_roles(p, PAGE_SIZE, s).await }
    });

    let (roles, total) = match data.value()() {
        None => {
            return rsx! {
                div { class: "flex justify-center p-12",
                    span { class: "loading loading-spinner loading-lg" }
                }
            }
        }
        Some(Ok(r)) => {
            load_error.set(None);
            (r.items, r.total)
        }
        Some(Err(e)) => {
            load_error.set(Some(e.to_string()));
            (vec![], 0i64)
        }
    };

    rsx! {
        div { class: "space-y-3",
            div { class: "flex items-center gap-3",
                input {
                    class: "input input-bordered input-sm w-full max-w-sm",
                    r#type: "text",
                    placeholder: "Search roles…",
                    value: "{search}",
                    oninput: move |e| {
                        search.set(e.value());
                        page.set(0);
                    },
                }
                span { class: "text-sm text-base-content/50 shrink-0", "{total} role(s)" }
            }

            if let Some(err) = load_error() {
                div { class: "alert alert-error text-sm font-mono", "{err}" }
            }

            div { class: "overflow-x-auto",
                table { class: "table w-full",
                    thead {
                        tr {
                            th { "Role" }
                            th { "Permissions" }
                        }
                    }
                    tbody {
                        for role in roles {
                            RoleRow {
                                key: "{role.id}",
                                role: role.clone(),
                                all_permissions: all_permissions.clone(),
                                on_change: move |_| *refresh.write() += 1,
                            }
                        }
                    }
                }
            }

            Pagination {
                page: page(),
                total,
                limit: PAGE_SIZE,
                on_page: move |p| page.set(p),
            }
        }
    }
}

#[component]
fn RoleRow(
    role: AdminRole,
    all_permissions: Vec<AdminPermission>,
    on_change: EventHandler<()>,
) -> Element {
    let unassigned: Vec<AdminPermission> = all_permissions
        .iter()
        .filter(|p| !role.permissions.iter().any(|rp| rp.id == p.id))
        .cloned()
        .collect();

    let mut selected_perm_id =
        use_signal(|| unassigned.first().map(|p| p.id).unwrap_or(0i32));

    rsx! {
        tr {
            td { class: "font-medium", "{role.name}" }
            td {
                div { class: "flex flex-wrap gap-1 items-center",
                    for perm in role.permissions.iter() {
                        {
                            let perm_id = perm.id;
                            let role_id = role.id;
                            let perm_name = perm.name.clone();
                            rsx! {
                                span { class: "badge badge-secondary gap-1",
                                    "{perm_name}"
                                    button {
                                        class: "btn btn-ghost btn-xs p-0 min-h-0 h-auto leading-none",
                                        onclick: move |_| {
                                            spawn(async move {
                                                let _ = admin_revoke_role_permission(role_id, perm_id).await;
                                                on_change.call(());
                                            });
                                        },
                                        "✕"
                                    }
                                }
                            }
                        }
                    }
                    if !unassigned.is_empty() {
                        div { class: "flex gap-1 items-center",
                            select {
                                class: "select select-xs select-bordered",
                                onchange: move |e: Event<FormData>| {
                                    if let Ok(id) = e.value().parse::<i32>() {
                                        selected_perm_id.set(id);
                                    }
                                },
                                for perm in &unassigned {
                                    option { value: "{perm.id}", "{perm.name}" }
                                }
                            }
                            button {
                                class: "btn btn-xs btn-success",
                                onclick: move |_| {
                                    let perm_id = selected_perm_id();
                                    let role_id = role.id;
                                    spawn(async move {
                                        let _ = admin_assign_role_permission(role_id, perm_id).await;
                                        on_change.call(());
                                    });
                                },
                                "+"
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── Invites tab ───────────────────────────────────────────────────────────────
//
// Links that give whoever opens them a role (after signing up, if they have no
// account). Only a hash of each link is stored, so a link is shown once, when made.

/// Role new invites default to: the Arcane dice test group.
const DEFAULT_INVITE_ROLE: &str = "arcane_user";

#[component]
fn InvitesTab(all_roles: Vec<AdminRole>) -> Element {
    // No fallback to another role: that could quietly make an invite for `admin`.
    let default_role = all_roles.iter().find(|r| r.name == DEFAULT_INVITE_ROLE).map(|r| r.id).unwrap_or(0);
    let mut role_id = use_signal(|| default_role);
    let mut days = use_signal(|| "7".to_string());
    let mut max_uses = use_signal(|| "1".to_string());
    let mut note = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut form_error = use_signal(|| Option::<String>::None);
    let mut new_link = use_signal(|| Option::<String>::None);
    let mut copied = use_signal(|| Option::<bool>::None);
    let mut refresh = use_signal(|| 0u32);

    let data = use_resource(move || {
        let _ = refresh();
        async move { admin_list_invites().await }
    });

    let create = move |evt: FormEvent| {
        evt.prevent_default();
        spawn(async move {
            let Ok(d) = days().trim().parse::<i64>() else {
                form_error.set(Some("Days must be a number.".into()));
                return;
            };
            let uses = match max_uses().trim() {
                "" => None,
                n => match n.parse::<i32>() {
                    Ok(n) => Some(n),
                    Err(_) => {
                        form_error.set(Some("“How many people” must be a number, or left empty for no limit.".into()));
                        return;
                    }
                },
            };
            if role_id() == 0 {
                form_error.set(Some("Choose which role the link gives.".into()));
                return;
            }
            let n = Some(note().trim().to_string()).filter(|n| !n.is_empty());
            busy.set(true);
            form_error.set(None);
            // So a failed attempt doesn't leave the previous link looking like the new one.
            new_link.set(None);
            match admin_create_invite(role_id(), d, uses, n).await {
                Ok(invite) => {
                    new_link.set(invite.link);
                    copied.set(None);
                    note.set(String::new());
                    *refresh.write() += 1;
                }
                Err(e) => form_error.set(Some(server_message(e))),
            }
            busy.set(false);
        });
    };

    let copy = move |_| {
        let Some(link) = new_link() else { return };
        spawn(async move {
            // eval hands back the script's return value; without one it reports an error.
            let js = format!(
                "try {{ await navigator.clipboard.writeText({}); return true; }} catch (e) {{ return false; }}",
                serde_json::Value::String(link)
            );
            let ok = document::eval(&js).await.ok().and_then(|v| v.as_bool()).unwrap_or(false);
            copied.set(Some(ok));
        });
    };

    rsx! {
        div { class: "space-y-6",
            form { class: "bg-base-100 rounded-lg p-4 space-y-3", onsubmit: create,
                h2 { class: "font-semibold", "New invite link" }
                div { class: "grid gap-3 sm:grid-cols-2",
                    label { class: "form-control",
                        span { class: "label-text mb-1", "Gives the role" }
                        select {
                            class: "select select-bordered select-sm",
                            onchange: move |e: Event<FormData>| {
                                if let Ok(id) = e.value().parse::<i32>() {
                                    role_id.set(id);
                                }
                            },
                            if role_id() == 0 {
                                option { value: "0", selected: true, disabled: true, "Choose a role…" }
                            }
                            for role in all_roles.iter() {
                                option { value: "{role.id}", selected: role.id == role_id(), "{role.name}" }
                            }
                        }
                    }
                    label { class: "form-control",
                        span { class: "label-text mb-1", "Note (who it's for)" }
                        input {
                            class: "input input-bordered input-sm",
                            r#type: "text",
                            maxlength: 100,
                            placeholder: "e.g. Sam, or the Discord group",
                            value: "{note}",
                            oninput: move |e| note.set(e.value()),
                        }
                    }
                    label { class: "form-control",
                        span { class: "label-text mb-1", "Works for (days)" }
                        input {
                            class: "input input-bordered input-sm",
                            r#type: "number",
                            min: 1,
                            max: 365,
                            required: true,
                            value: "{days}",
                            oninput: move |e| days.set(e.value()),
                        }
                    }
                    label { class: "form-control",
                        span { class: "label-text mb-1", "How many people can use it (empty = no limit)" }
                        input {
                            class: "input input-bordered input-sm",
                            r#type: "number",
                            min: 1,
                            max: 1000,
                            value: "{max_uses}",
                            oninput: move |e| max_uses.set(e.value()),
                        }
                    }
                }
                button { class: "btn btn-primary btn-sm", r#type: "submit", disabled: busy(), "Make link" }
                if let Some(e) = form_error() {
                    div { class: "alert alert-error text-sm", "{e}" }
                }
                if let Some(link) = new_link() {
                    div { class: "alert alert-success flex-col items-stretch gap-2",
                        span { class: "text-sm",
                            "Copy it now: it won't be shown again. Send it to the people you're inviting. "
                            "Don't open it yourself while logged in: that would use it."
                        }
                        div { class: "flex gap-2",
                            input { class: "input input-bordered input-sm w-full font-mono", readonly: true, value: "{link}" }
                            button { class: "btn btn-sm", r#type: "button", onclick: copy,
                                if copied() == Some(true) { "Copied" } else { "Copy" }
                            }
                        }
                        if copied() == Some(false) {
                            span { class: "text-sm", "Couldn't copy it: select the link and copy it yourself." }
                        }
                    }
                }
            }

            match data.value()() {
                None => rsx! {
                    div { class: "flex justify-center p-12", span { class: "loading loading-spinner loading-lg" } }
                },
                Some(Err(e)) => rsx! {
                    div { class: "alert alert-error text-sm font-mono", "{e}" }
                },
                Some(Ok(invites)) if invites.is_empty() => rsx! {
                    p { class: "text-sm opacity-60", "No invite links yet." }
                },
                Some(Ok(invites)) => rsx! {
                    div { class: "overflow-x-auto",
                        table { class: "table w-full",
                            thead {
                                tr {
                                    th { "Note" }
                                    th { "Role" }
                                    th { "Used" }
                                    th { "Until (UTC)" }
                                    th { "Status" }
                                    th {}
                                }
                            }
                            tbody {
                                for invite in invites {
                                    InviteRow { key: "{invite.id}", invite: invite.clone(), on_change: move |_| *refresh.write() += 1 }
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}

/// An invite's status as the list shows it.
fn invite_status(invite: &AdminInvite) -> &'static str {
    if invite.revoked {
        "Turned off"
    } else if invite.live {
        "Active"
    } else if invite.max_uses.is_some_and(|m| invite.uses >= m) {
        "Used up"
    } else {
        "Expired"
    }
}

#[component]
fn InviteRow(invite: AdminInvite, on_change: EventHandler<()>) -> Element {
    // Turning a link off can't be undone: the first click asks.
    let mut confirming = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);

    let used = match invite.max_uses {
        Some(max) => format!("{} / {max}", invite.uses),
        None => invite.uses.to_string(),
    };
    // RFC 3339: the date is the first 10 characters.
    let until = invite.expires_at.get(..10).unwrap_or(&invite.expires_at).to_string();
    let status = invite_status(&invite);
    let badge = if status == "Active" { "badge badge-success" } else { "badge badge-ghost" };
    let id = invite.id;
    rsx! {
        tr {
            td {
                div { class: "font-medium", { invite.note.as_deref().unwrap_or("—") } }
                if let Some(by) = invite.created_by.as_deref() {
                    div { class: "text-xs opacity-60", "by {by}" }
                }
                if !invite.joined.is_empty() {
                    div { class: "text-xs opacity-60", "joined: {invite.joined.join(\", \")}" }
                }
            }
            td { span { class: "badge badge-primary", "{invite.role}" } }
            td { "{used}" }
            td { "{until}" }
            td { span { class: badge, "{status}" } }
            td {
                if invite.live {
                    if confirming() {
                        div { class: "flex gap-1",
                            button {
                                class: "btn btn-error btn-xs",
                                disabled: busy(),
                                onclick: move |_| {
                                    spawn(async move {
                                        busy.set(true);
                                        match admin_revoke_invite(id).await {
                                            Ok(()) => on_change.call(()),
                                            Err(e) => error.set(Some(server_message(e))),
                                        }
                                        busy.set(false);
                                        confirming.set(false);
                                    });
                                },
                                "Yes, turn off"
                            }
                            button { class: "btn btn-ghost btn-xs", onclick: move |_| confirming.set(false), "Keep" }
                        }
                    } else {
                        button {
                            class: "btn btn-ghost btn-xs",
                            title: "Stop this link from letting anyone else in. People who joined keep the role.",
                            onclick: move |_| {
                                error.set(None);
                                confirming.set(true);
                            },
                            "Turn off"
                        }
                    }
                }
                if let Some(e) = error() {
                    div { class: "text-xs text-error mt-1", "Not turned off: {e}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite(revoked: bool, live: bool, uses: i32, max_uses: Option<i32>) -> AdminInvite {
        AdminInvite {
            id: 1,
            role_id: 1,
            role: "arcane_user".into(),
            note: None,
            created_by: None,
            created_at: String::new(),
            expires_at: String::new(),
            max_uses,
            uses,
            revoked,
            live,
            joined: vec![],
            link: None,
        }
    }

    #[test]
    fn invite_statuses() {
        assert_eq!(invite_status(&invite(false, true, 0, Some(1))), "Active");
        assert_eq!(invite_status(&invite(false, false, 1, Some(1))), "Used up");
        assert_eq!(invite_status(&invite(false, false, 0, None)), "Expired");
        // Turned off wins, even when also used up.
        assert_eq!(invite_status(&invite(true, false, 1, Some(1))), "Turned off");
    }
}

// ── Shared pagination control ─────────────────────────────────────────────────

#[component]
fn Pagination(page: u32, total: i64, limit: u32, on_page: EventHandler<u32>) -> Element {
    let total_pages = ((total as f64) / (limit as f64)).ceil() as u32;
    if total_pages <= 1 {
        return rsx! {};
    }

    // Show at most 7 page buttons: always first, last, current ± 2, with ellipses.
    let mut buttons: Vec<Option<u32>> = vec![];
    for i in 0..total_pages {
        let near_start = i < 2;
        let near_end = i >= total_pages.saturating_sub(2);
        let near_current = i.abs_diff(page) <= 2;
        if near_start || near_end || near_current {
            buttons.push(Some(i));
        } else if buttons.last() != Some(&None) {
            buttons.push(None); // ellipsis placeholder
        }
    }

    rsx! {
        div { class: "flex justify-center items-center gap-1 mt-2",
            button {
                class: "btn btn-sm btn-ghost",
                disabled: page == 0,
                onclick: move |_| on_page.call(page.saturating_sub(1)),
                "‹"
            }
            for btn in buttons {
                match btn {
                    None => rsx! { span { class: "px-1 text-base-content/40", "…" } },
                    Some(i) => rsx! {
                        button {
                            class: if i == page { "btn btn-sm btn-primary" } else { "btn btn-sm btn-ghost" },
                            "data-trace-name": "page",
                            onclick: move |_| on_page.call(i),
                            "{i + 1}"
                        }
                    },
                }
            }
            button {
                class: "btn btn-sm btn-ghost",
                disabled: page + 1 >= total_pages,
                onclick: move |_| on_page.call(page + 1),
                "›"
            }
        }
    }
}
