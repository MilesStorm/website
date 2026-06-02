use dioxus::prelude::*;

use api::{
    admin_assign_role_permission, admin_assign_user_role, admin_list_all_roles,
    admin_list_permissions, admin_list_roles, admin_list_users, admin_revoke_role_permission,
    admin_revoke_user_role, AdminPermission, AdminRole, AdminUser,
};
use ui::data_dir::LoginStatus;

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
                }

                match tab() {
                    Tab::Users => rsx! { UsersTab { all_roles } },
                    Tab::Roles => rsx! { RolesTab { all_permissions } },
                }
            }
        }
    }
}

// ── Users tab ─────────────────────────────────────────────────────────────────

#[component]
fn UsersTab(all_roles: Vec<AdminRole>) -> Element {
    let mut search = use_signal(|| String::new());
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
    let mut search = use_signal(|| String::new());
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
