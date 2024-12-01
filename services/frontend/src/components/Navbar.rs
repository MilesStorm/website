use dioxus::prelude::*;
use dioxus_elements::{div, span};

use crate::{
    components::icon::{default_profile_picture, Logo_c},
    hooks::{has_permission, logout, LogInStatus},
    LOGIN_STATUS,
};

#[component]
fn valheim_button() -> Element {
    let is_permitted = use_resource(move || async move { has_permission("restart_valheim").await });

    match is_permitted() {
        Some(permission) => {
            if permission {
                rsx! {
                    li{ Link {to: "/valheim", "Valheim" } }
                }
            } else {
                rsx! {}
            }
        }
        None => rsx! {
            span {
                class: "loading loading-spinner loading-xs"
            }
        },
    }
}

#[component]
pub fn Navbar() -> Element {
    let user = LOGIN_STATUS.read().clone();

    let logout_action = move |_| {
        spawn(async move {
            logout().await;
        });
    };

    rsx! {
        header { class: "sticky top-0 z-50 navbar bg-base-200 shadow-xl rounded-box",
            div { class: "navbar-start",
                div { class: "dropdown",
                    label { class: "btn btn-ghost lg:hidden",
                        tabindex: "0",
                        svg {
                            xmlns: "http://www.w3.org/2000/svg",
                            class: "h-5 w-5",
                            "fill": "none",
                            view_box: "0 0 24 24",
                            "stroke": "currentColor",
                            path {
                                "stroke-linecap": "round",
                                "stroke-linejoin": "round",
                                "stroke-width": "2",
                                d: "M4 6h16M4 12h8m-8 6h16"
                            }
                        }
                    }
                    ul { tabindex: "0",
                        class: "menu menu-compact dropdown-content mt-3 p-2 shadow bg-base-200 rounded-box w-52",
                        li { Link {to: "/", "Home" } }
                        li { Link {to: "/arcaneeye", "ArcaneEye" } }
                        li{ Link {to: "/valheim", "Valheim" } }
                    }
                }
                Link { class: "btn btn-ghost btn-circle avatar",
                    to: "/",
                    Logo_c { width: 40, height: 40, class: "" }
                }
            }
            div { class: "navbar-center hidden lg:flex",
                ul { class: "menu menu-horizontal px-1",
                    li{ Link {to: "/", "Home" } }
                    li{ Link {to: "/arcaneeye", "ArcaneEye" } }
                    valheim_button {}
                }
            }
            div{ class: "navbar-end",
                div { class: "flex flex-row gap-2",
                    div { class: "form-control hidden lg:flex",
                        input { r#type: "text", placeholder: "Seach", class: "input input-bordered" }
                    }
                    div {class: "",
                        button { class: "btn btn-ghost btn-circle lg:hidden",
                            svg {
                                xmlns: "http://www.w3.org/2000/svg",
                                class: "h-5 w-5",
                                "fill": "none",
                                view_box: "0 0 24 24",
                                "stroke": "currentColor",
                                path {
                                    "stroke-linecap": "round",
                                    "stroke-linejoin": "round",
                                    "stroke-width": "2",
                                    d: "M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z"
                                }
                            }
                        }
                    }
                    div {class: "dropdown dropdown-end",
                        label { tabindex: "0", class: "btn btn-ghost btn-circle avatar",
                            div { class: "w-10 rounded-full ring ring-primary ring-offset-base-100 ring-offset-2",
                                default_profile_picture {width: 40, height: 40}
                            }
                        }
                        ul {
                            tabindex: "0",
                            class: "mt-3 p-2 shadow menu menu-compact dropdown-content bg-base-200 rounded-box w-52",
                            match user {
                                LogInStatus::LoggedOut => rsx! {
                                    li { Link { to: "/login", "Login" } }
                                    li { Link {class: "underline font-bold", to: "/register", "Register" } }
                                },
                                LogInStatus::LoggedIn(user) => rsx! {
                                    li { Link { to: "/profile", "Profile: {user}" } }
                                    li { Link { to: "/settings", "Settings" } }
                                    li { p {onclick: logout_action, "Logout" } }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
