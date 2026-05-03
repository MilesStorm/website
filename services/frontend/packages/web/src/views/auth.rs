use dioxus::prelude::*;

use api::{get_oauth_init_url, login_password, register_password};

use crate::LOGIN_STATUS;

// ---- Login ----

#[component]
pub fn Login(error: String) -> Element {
    let github_url = use_server_future(|| get_oauth_init_url("github".to_string()))?;
    let google_url = use_server_future(|| get_oauth_init_url("google".to_string()))?;

    let mut login_error = use_signal(String::new);

    let handle_login = move |evt: FormEvent| {
        evt.prevent_default();
        spawn(async move {
            let username = form_text(&evt, "username");
            let password = form_text(&evt, "password");

            match login_password(username, password).await {
                Ok(status) => {
                    *LOGIN_STATUS.write() = status;
                    navigator().push("/");
                }
                Err(e) => login_error.set(e.to_string()),
            }
        });
    };

    rsx! {
        div { class: "h-[calc(100vh-5rem)] flex items-center justify-center flex-col",
            div { class: "bg-base-200 p-8 rounded-lg shadow-lg max-w-md w-full",
                h2 { class: "text-2xl font-bold text-center mb-8", "Log In" }

                // OAuth buttons
                div { class: "space-y-4 flex flex-col mb-4",
                    if let Some(Ok(url)) = github_url.value()() {
                        a { href: "{url}", class: "btn bg-black text-white border-black",
                            github_icon {}
                            "Login with GitHub"
                        }
                    } else {
                        button { class: "btn btn-disabled", "Login with GitHub" }
                    }
                    if let Some(Ok(url)) = google_url.value()() {
                        a { href: "{url}", class: "btn bg-white text-black border-[#e5e5e5]",
                            google_icon {}
                            "Login with Google"
                        }
                    } else {
                        button { class: "btn btn-disabled", "Login with Google" }
                    }
                }

                div { class: "divider", "OR" }

                // Password form
                form { onsubmit: handle_login,
                    div { class: "space-y-4",
                        div {
                            label { class: "sr-only", r#for: "username", "Username" }
                            input {
                                r#type: "text",
                                name: "username",
                                placeholder: "Username",
                                class: "input input-bordered w-full",
                                id: "username"
                            }
                        }
                        div {
                            label { class: "sr-only", r#for: "password", "Password" }
                            input {
                                r#type: "password",
                                name: "password",
                                placeholder: "Password",
                                class: "input input-bordered w-full",
                                id: "password"
                            }
                        }
                        button { r#type: "submit", class: "btn btn-primary w-full", "Log In" }
                    }
                }

                // Error alerts
                if !login_error().is_empty() {
                    div { class: "alert alert-error mt-4",
                        span { "{login_error()}" }
                    }
                }
                if error.contains("email_exists") {
                    div { class: "alert alert-warning mt-4",
                        span { "An account with this email already exists. Try logging in instead." }
                    }
                }
            }
        }
    }
}

// ---- Register ----

#[component]
pub fn Register() -> Element {
    let github_url = use_server_future(|| get_oauth_init_url("github".to_string()))?;
    let google_url = use_server_future(|| get_oauth_init_url("google".to_string()))?;

    let mut reg_error = use_signal(String::new);

    let handle_register = move |evt: FormEvent| {
        evt.prevent_default();
        spawn(async move {
            let username = form_text(&evt, "username");
            let email = form_text(&evt, "email");
            let password = form_text(&evt, "password");

            match register_password(username, email, password).await {
                Ok(status) => {
                    *LOGIN_STATUS.write() = status;
                    navigator().push("/");
                }
                Err(e) => reg_error.set(e.to_string()),
            }
        });
    };

    rsx! {
        div { class: "h-[calc(100vh-5rem)] flex items-center justify-center",
            div { class: "p-8 bg-base-200 shadow-lg rounded-lg max-w-md w-full",
                h2 { class: "text-center text-2xl font-bold mb-6", "Sign Up" }

                div { class: "flex flex-col space-y-4 mb-6",
                    if let Some(Ok(url)) = github_url.value()() {
                        a { href: "{url}", class: "btn btn-outline btn-accent w-full",
                            "Sign up with GitHub"
                        }
                    } else {
                        button { class: "btn btn-disabled", "Sign up with GitHub" }
                    }
                    if let Some(Ok(url)) = google_url.value()() {
                        a { href: "{url}", class: "btn btn-outline btn-accent w-full",
                            "Sign up with Google"
                        }
                    } else {
                        button { class: "btn btn-disabled", "Sign up with Google" }
                    }
                }

                div { class: "divider", "OR" }

                form { onsubmit: handle_register,
                    div { class: "mb-4",
                        div { class: "label",
                            span { class: "label-text", "Email" }
                            if reg_error().contains("email") {
                                span { class: "label-text-alt text-error", "Email already in use" }
                            }
                        }
                        input {
                            r#type: "email",
                            name: "email",
                            class: "input input-bordered w-full",
                            class: if reg_error().contains("email") { "input-error" }
                        }
                    }
                    div { class: "mb-4",
                        div { class: "label",
                            span { class: "label-text", "Username" }
                            if reg_error().contains("User") || reg_error().contains("username") {
                                span { class: "label-text-alt text-error", "Username already in use" }
                            }
                        }
                        input {
                            r#type: "text",
                            name: "username",
                            class: "input input-bordered w-full",
                            class: if reg_error().contains("User") { "input-error" }
                        }
                    }
                    div { class: "mb-6",
                        label { class: "block mb-2 text-sm font-medium", r#for: "password", "Password" }
                        input {
                            r#type: "password",
                            name: "password",
                            class: "input input-bordered w-full",
                            id: "password"
                        }
                    }
                    button { r#type: "submit", class: "btn btn-primary w-full", "Sign up with Email" }
                }

                if !reg_error().is_empty() && !reg_error().contains("email") && !reg_error().contains("User") {
                    div { class: "alert alert-error mt-4",
                        span { "{reg_error()}" }
                    }
                }
            }
        }
    }
}

fn form_text(evt: &FormData, name: &str) -> String {
    match evt.get_first(name) {
        Some(FormValue::Text(s)) => s,
        _ => String::new(),
    }
}

// ---- SVG helpers ----

#[component]
fn github_icon() -> Element {
    rsx! {
        svg { height: "16", view_box: "0 0 24 24", width: "16", xmlns: "http://www.w3.org/2000/svg",
            path {
                d: "M12,2A10,10 0 0,0 2,12C2,16.42 4.87,20.17 8.84,21.5C9.34,21.58 9.5,21.27 9.5,21C9.5,20.77 9.5,20.14 9.5,19.31C6.73,19.91 6.14,17.97 6.14,17.97C5.68,16.81 5.03,16.5 5.03,16.5C4.12,15.88 5.1,15.9 5.1,15.9C6.1,15.97 6.63,16.93 6.63,16.93C7.5,18.45 8.97,18 9.54,17.76C9.63,17.11 9.89,16.67 10.17,16.42C7.95,16.17 5.62,15.31 5.62,11.5C5.62,10.39 6,9.5 6.65,8.79C6.55,8.54 6.2,7.5 6.75,6.15C6.75,6.15 7.59,5.88 9.5,7.17C10.29,6.95 11.15,6.84 12,6.84C12.85,6.84 13.71,6.95 14.5,7.17C16.41,5.88 17.25,6.15 17.25,6.15C17.8,7.5 17.45,8.54 17.35,8.79C18,9.5 18.38,10.39 18.38,11.5C18.38,15.32 16.04,16.16 13.81,16.41C14.17,16.72 14.5,17.33 14.5,18.26C14.5,19.6 14.5,20.68 14.5,21C14.5,21.27 14.66,21.59 15.17,21.5C19.14,20.16 22,16.42 22,12A10,10 0 0,0 12,2Z",
                fill: "white"
            }
        }
    }
}

#[component]
fn google_icon() -> Element {
    rsx! {
        svg { height: "16", view_box: "0 0 512 512", width: "16", xmlns: "http://www.w3.org/2000/svg",
            g {
                path { d: "m0 0H512V512H0", fill: "#fff" }
                path { d: "M153 292c30 82 118 95 171 60h62v48A192 192 0 0190 341", fill: "#34a853" }
                path { d: "m386 400a140 175 0 0053-179H260v74h102q-7 37-38 57", fill: "#4285f4" }
                path { d: "m90 341a208 200 0 010-171l63 49q-12 37 0 73", fill: "#fbbc02" }
                path { d: "m153 219c22-69 116-109 179-50l55-54c-78-75-230-72-297 55", fill: "#ea4335" }
            }
        }
    }
}
