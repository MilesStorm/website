use dioxus::prelude::*;

use crate::{
    components::icon::{default_profile_picture, Logo_c},
    utils::LogInStatus,
    LOGIN_STATUS,
};

#[component]
pub fn Navbar() -> Element {
    // let is_hidden = use_state(cx , || "hidden");

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
                            li {
                                Link { class: "justify-between",
                                 to:"/profile",
                                "Profile"
                                span { class: "badge", "New" }
                                }
                            }
                            li { a { class: "justify-between","Settings" } }
                            li { a {"test"} }
                            li {
                                match *LOGIN_STATUS.read() {
                                    LogInStatus::LoggedOut => rsx! {a { class: "justify-between", key: "{LOGIN_STATUS}", Link { to: "/login", "Log in"} }},
                                    LogInStatus::LoggedIn(_) => rsx! {
                                        a { class: "justify-between", key: "{LOGIN_STATUS}",
                                            onclick: move |_evt| {
                                            // LOGIN_STATUS.write(LogInStatus::LoggedOut);
                                        },
                                    "Logout"
                                    }}
                                }
                                // if login_status == LogInStatus::LoggedOut {
                                //     a { class: "justify-between", "Login" }
                                // } else {
                                //     a { class: "justify-between", "Logout" }
                                // }
                            }
                        }
                    }
                }
            }
        }
    }
}
