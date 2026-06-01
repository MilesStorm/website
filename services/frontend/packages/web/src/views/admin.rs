use dioxus::prelude::*;

use api::{
    admin_assign_role_permission, admin_assign_user_role, admin_list_permissions,
    admin_list_roles, admin_list_users, admin_revoke_role_permission, admin_revoke_user_role,
    AdminPermission, AdminRole, AdminUser,
};
use ui::data_dir::LoginStatus;

use crate::{LOGIN_STATUS, PERMISSIONS};

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
    let mut users: Signal<Vec<AdminUser>> = use_signal(Vec::new);
    let mut roles: Signal<Vec<AdminRole>> = use_signal(Vec::new);
    let mut all_permissions: Signal<Vec<AdminPermission>> = use_signal(Vec::new);
    let mut load_errors: Signal<Vec<String>> = use_signal(Vec::new);
    let mut refresh = use_signal(|| 0u32);

    let data = use_resource(move || async move {
        let _ = refresh();
        let u = admin_list_users().await;
        let r = admin_list_roles().await;
        let p = admin_list_permissions().await;
        (u, r, p)
    });

    use_effect(move || {
        if let Some((u, r, p)) = data.value()() {
            let mut errs = Vec::new();
            match u {
                Ok(v) => users.set(v),
                Err(e) => errs.push(format!("users: {e}")),
            }
            match r {
                Ok(v) => roles.set(v),
                Err(e) => errs.push(format!("roles: {e}")),
            }
            match p {
                Ok(v) => all_permissions.set(v),
                Err(e) => errs.push(format!("permissions: {e}")),
            }
            load_errors.set(errs);
        }
    });

    rsx! {
        div { class: "container mx-auto mt-10 px-4",
            div { class: "bg-base-200 p-8 rounded-lg shadow-lg",
                h1 { class: "text-2xl font-bold mb-6", "Admin Panel" }

                // Surface any server-function errors so they're not silently swallowed
                if !load_errors().is_empty() {
                    div { class: "alert alert-error mb-4",
                        div {
                            span { class: "font-bold", "Failed to load data:" }
                            ul { class: "list-disc list-inside mt-1",
                                for err in load_errors() {
                                    li { class: "font-mono text-sm", "{err}" }
                                }
                            }
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

                if data.value()().is_none() {
                    div { class: "flex justify-center p-12",
                        span { class: "loading loading-spinner loading-lg" }
                    }
                } else {
                    match tab() {
                        Tab::Users => rsx! {
                            UsersTab {
                                users: users(),
                                all_roles: roles(),
                                on_change: move |_| *refresh.write() += 1,
                            }
                        },
                        Tab::Roles => rsx! {
                            RolesTab {
                                roles: roles(),
                                all_permissions: all_permissions(),
                                on_change: move |_| *refresh.write() += 1,
                            }
                        },
                    }
                }
            }
        }
    }
}

#[component]
fn UsersTab(
    users: Vec<AdminUser>,
    all_roles: Vec<AdminRole>,
    on_change: EventHandler<()>,
) -> Element {
    rsx! {
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
                            on_change: move |_| on_change.call(()),
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn UserRow(
    user: AdminUser,
    all_roles: Vec<AdminRole>,
    on_change: EventHandler<()>,
) -> Element {
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

#[component]
fn RolesTab(
    roles: Vec<AdminRole>,
    all_permissions: Vec<AdminPermission>,
    on_change: EventHandler<()>,
) -> Element {
    rsx! {
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
                            on_change: move |_| on_change.call(()),
                        }
                    }
                }
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
