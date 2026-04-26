use dioxus::prelude::*;

use crate::{
    components::icon::{default_profile_picture, logo_c},
    data_dir::LoginStatus,
};

const NAVBAR_CSS: Asset = asset!("/assets/styling/navbar.css");

/// Thin wrapper that provides nav links from the platform crate as children.
#[component]
pub fn Navbarr(children: Element) -> Element {
    rsx! {
        document::Link { rel: "stylesheet", href: NAVBAR_CSS }
        div { id: "navbar", {children} }
    }
}

/// Full DaisyUI navbar. `user` drives the profile dropdown.
/// `on_logout` is called when the user clicks Logout.
#[component]
pub fn Navbar(user: LoginStatus, on_logout: EventHandler<()>) -> Element {
    rsx! {
        header { class: "sticky top-0 z-50 navbar bg-base-200 shadow-xl rounded-box",

            // ---- Start: logo + mobile hamburger ----
            div { class: "navbar-start",
                div { class: "dropdown",
                    label { class: "btn btn-ghost lg:hidden", tabindex: "0",
                        svg {
                            xmlns: "http://www.w3.org/2000/svg",
                            class: "h-5 w-5",
                            fill: "none",
                            view_box: "0 0 24 24",
                            stroke: "currentColor",
                            path {
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                stroke_width: "2",
                                d: "M4 6h16M4 12h8m-8 6h16"
                            }
                        }
                    }
                    ul { tabindex: "0",
                        class: "menu menu-compact dropdown-content mt-3 p-2 shadow bg-base-200 rounded-box w-52",
                        li { Link { to: "/", "Home" } }
                        match &user {
                            LoginStatus::LoggedIn(_) => rsx! {
                                li { Link { to: "/ark", "Ark" } }
                                li { Link { to: "/profile", "Profile" } }
                            },
                            LoginStatus::LoggedOut => rsx! {}
                        }
                    }
                }
                Link { class: "btn btn-ghost btn-circle avatar", to: "/",
                    logo_c { width: 40, height: 40, class: "" }
                }
            }

            // ---- Center: desktop nav links ----
            div { class: "navbar-center hidden lg:flex",
                ul { class: "menu menu-horizontal px-1",
                    li { Link { to: "/", "Home" } }
                    match &user {
                        LoginStatus::LoggedIn(_) => rsx! {
                            li { Link { to: "/ark", "Ark" } }
                        },
                        LoginStatus::LoggedOut => rsx! {}
                    }
                }
            }

            // ---- End: search + profile dropdown ----
            div { class: "navbar-end",
                div { class: "flex flex-row gap-2",
                    div { class: "form-control hidden lg:flex",
                        input { r#type: "text", placeholder: "Search", class: "input input-bordered" }
                    }
                    div { class: "dropdown dropdown-end",
                        label { tabindex: "0", class: "btn btn-ghost btn-circle avatar",
                            div { class: "w-10 rounded-full ring ring-primary ring-offset-base-100 ring-offset-2",
                                default_profile_picture { width: 40, height: 40 }
                            }
                        }
                        ul { tabindex: "0",
                            class: "mt-3 p-2 shadow menu menu-compact dropdown-content bg-base-200 rounded-box w-52",
                            match &user {
                                LoginStatus::LoggedOut => rsx! {
                                    li { Link { to: "/login", "Login" } }
                                    li { Link { class: "underline font-bold", to: "/register", "Register" } }
                                },
                                LoginStatus::LoggedIn(username) => rsx! {
                                    li { Link { to: "/profile", "Profile: {username}" } }
                                    li {
                                        button {
                                            onclick: move |_| on_logout.call(()),
                                            "Logout"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
