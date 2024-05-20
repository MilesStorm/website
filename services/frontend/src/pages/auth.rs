use dioxus::prelude::*;

use crate::hooks::{github_oauth, google_oauth, login, register, LogInStatus};

#[component]
pub fn Login(error: String) -> Element {
    let _ = use_signal(|| String::new());

    let google_oauth_call = move |_| {
        spawn(async move {
            let res = google_oauth().await;

            tracing::info!("OAuth response: {:?}", res);
            match res {
                Ok(url) => {
                    tracing::info!("OAuth success: {:?}", url);
                    navigator().push(url.as_str());
                }
                Err(e) => tracing::warn!("OAuth failed: {:?}", e),
            }
        });
    };

    let oauth_call = move |_| {
        spawn(async move {
            let res = github_oauth().await;

            tracing::info!("OAuth response: {:?}", res);
            match res {
                Ok(url) => {
                    tracing::info!("OAuth success: {:?}", url);
                    navigator().push(url.as_str());
                }
                Err(e) => tracing::warn!("OAuth failed: {:?}", e),
            }
        });
    };

    let component_login = move |evt: Event<FormData>| {
        spawn(async move {
            let form_data = evt.values();
            let username = form_data.get("username").unwrap().as_value();
            let password = form_data.get("password").unwrap().as_value();
            let reg_attempt = login(&username, &password).await;

            match reg_attempt {
                Ok(att) => {
                    LogInStatus::set_logged_in().await;
                    if let Some(_) = att.1 {
                        navigator().push("/");
                    };
                }
                Err(e) => tracing::warn!("login failed: {:?}", e),
            }
        });
    };

    rsx! {
        div {
            div {
                class:"min-h-screen flex items-center justify-center flex-col",
                div {
                    class:"bg-base-200 p-8 rounded-lg shadow-lg max-w-md w-full",
                    h2 {
                        class:"text-2xl font-bold text-center mb-8","Log In"
                    } div {
                        class:"space-y-4",
                        button {
                            onclick: google_oauth_call,
                            class:"btn btn-outline btn-accent w-full flex items-center justify-center gap-2",img {
                                src:"https://upload.wikimedia.org/wikipedia/commons/archive/c/c1/20230822192910%21Google_%22G%22_logo.svg",alt:"Google logo",class:"w-6 h-6"
                            }"Log in with Google"
                        }
                            button {
                            onclick: oauth_call,
                            class:"btn btn-outline btn-accent w-full flex items-center justify-center gap-2",svg {
                                "xmlns":"http://www.w3.org/2000/svg",width:"16","viewBox":"0 0 16 17",height:"17","preserveAspectRatio":"none",class:"flex-grow-0 flex-shrink-0 fill-black dark:fill-white",path {
                                    "d":"M5.43437 13.1124C5.43437 13.1749 5.3625 13.2249 5.27187 13.2249C5.16875 13.2343 5.09688 13.1843 5.09688 13.1124C5.09688 13.0499 5.16875 12.9999 5.25938 12.9999C5.35313 12.9905 5.43437 13.0405 5.43437 13.1124ZM4.4625 12.9718C4.44063 13.0343 4.50313 13.1061 4.59688 13.1249C4.67813 13.1561 4.77187 13.1249 4.79063 13.0624C4.80938 12.9999 4.75 12.928 4.65625 12.8999C4.575 12.878 4.48438 12.9093 4.4625 12.9718ZM5.84375 12.9186C5.75313 12.9405 5.69062 12.9999 5.7 13.0718C5.70937 13.1343 5.79063 13.1749 5.88438 13.153C5.975 13.1311 6.0375 13.0718 6.02812 13.0093C6.01875 12.9499 5.93437 12.9093 5.84375 12.9186ZM7.9 0.943634C3.56563 0.943634 0.25 4.23426 0.25 8.56863C0.25 12.0343 2.43125 14.9999 5.54688 16.0436C5.94688 16.1155 6.0875 15.8686 6.0875 15.6655C6.0875 15.4718 6.07812 14.403 6.07812 13.7468C6.07812 13.7468 3.89062 14.2155 3.43125 12.8155C3.43125 12.8155 3.075 11.9061 2.5625 11.6718C2.5625 11.6718 1.84687 11.1811 2.6125 11.1905C2.6125 11.1905 3.39062 11.253 3.81875 11.9968C4.50312 13.203 5.65 12.8561 6.09688 12.6499C6.16875 12.1499 6.37188 11.803 6.59688 11.5968C4.85 11.403 3.0875 11.1499 3.0875 8.14363C3.0875 7.28426 3.325 6.85301 3.825 6.30301C3.74375 6.09988 3.47813 5.26238 3.90625 4.18113C4.55937 3.97801 6.0625 5.02488 6.0625 5.02488C6.6875 4.84988 7.35938 4.75926 8.025 4.75926C8.69063 4.75926 9.3625 4.84988 9.9875 5.02488C9.9875 5.02488 11.4906 3.97488 12.1438 4.18113C12.5719 5.26551 12.3063 6.09988 12.225 6.30301C12.725 6.85613 13.0312 7.28738 13.0312 8.14363C13.0312 11.1593 11.1906 11.3999 9.44375 11.5968C9.73125 11.8436 9.975 12.3124 9.975 13.0468C9.975 14.0999 9.96562 15.403 9.96562 15.6593C9.96562 15.8624 10.1094 16.1093 10.5063 16.0374C13.6313 14.9999 15.75 12.0343 15.75 8.56863C15.75 4.23426 12.2344 0.943634 7.9 0.943634ZM3.2875 11.7218C3.24687 11.753 3.25625 11.8249 3.30938 11.8843C3.35938 11.9343 3.43125 11.9561 3.47187 11.9155C3.5125 11.8843 3.50312 11.8124 3.45 11.753C3.4 11.703 3.32812 11.6811 3.2875 11.7218ZM2.95 11.4686C2.92813 11.5093 2.95937 11.5593 3.02187 11.5905C3.07187 11.6218 3.13438 11.6124 3.15625 11.5686C3.17812 11.528 3.14687 11.478 3.08437 11.4468C3.02187 11.428 2.97187 11.4374 2.95 11.4686ZM3.9625 12.5811C3.9125 12.6218 3.93125 12.7155 4.00312 12.7749C4.075 12.8468 4.16562 12.8561 4.20625 12.8061C4.24688 12.7655 4.22813 12.6718 4.16563 12.6124C4.09688 12.5405 4.00313 12.5311 3.9625 12.5811ZM3.60625 12.1218C3.55625 12.153 3.55625 12.2343 3.60625 12.3061C3.65625 12.378 3.74062 12.4093 3.78125 12.378C3.83125 12.3374 3.83125 12.2561 3.78125 12.1843C3.7375 12.1124 3.65625 12.0811 3.60625 12.1218Z",
                                }
                            }"Log in with GitHub"
                        }
                    }
                        div {
                        class:"divider","OR"
                    }
                        form {
                            onsubmit: component_login,
                            div {
                            class:"space-y-4",
                                div {
                                label {
                                    r#for:"username",class:"sr-only","Username"
                                }
                                input {
                                    r#type:"username",placeholder:"Username",required:"false",name:"username",class:"input input-bordered w-full",id:"username"
                                }
                            }
                                div {
                                    label {
                                        r#for:"password",class:"sr-only","Password"
                                    }input {
                                        required:"false",r#type:"password",name:"password",placeholder:"Password",class:"input input-bordered w-full",id:"password"
                                }
                            } button {
                                r#type:"submit",class:"btn btn-primary w-full","\n          Log In\n        "
                            }
                        }
                    }
                }
                if error.contains("email_exists") {
                    div {
                        class: "alert alert-warning container my-8 max-w-md w-full",
                        svg {
                            "xmlns": "http://www.w3.org/2000/svg",
                            class: "stroke-current shrink-0 h-6 w-6",
                            "fill": "none",
                            "viewBox": "0 0 24 24",
                            path {
                                "stroke-linecap": "round",
                                "stroke-linejoin": "round",
                                "stroke-width": "2",
                                "d": "M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"
                            }
                        }
                        span { "Email already exists! Try logging in instead" }
                    }
                }
            }
        }
    }
}

#[component]
pub fn Register() -> Element {
    let mut req_res = use_signal(|| String::new());

    let google_oauth_call = move |_| {
        spawn(async move {
            let res = google_oauth().await;

            tracing::info!("OAuth response: {:?}", res);
            match res {
                Ok(url) => {
                    tracing::info!("OAuth success: {:?}", url);
                    navigator().push(url.as_str());
                }
                Err(e) => tracing::warn!("OAuth failed: {:?}", e),
            }
        });
    };

    let oauth_call = move |_| {
        spawn(async move {
            let res = github_oauth().await;

            tracing::info!("OAuth response: {:?}", res);
            match res {
                Ok(url) => {
                    tracing::info!("OAuth success: {:?}", url);
                    navigator().push(url.as_str());
                }
                Err(e) => tracing::warn!("OAuth failed: {:?}", e),
            }
        });
    };

    let component_register = move |evt: Event<FormData>| {
        spawn(async move {
            let form_data = evt.values();
            let username = form_data.get("username").unwrap().as_value();
            let email = form_data.get("email").unwrap().as_value();
            let password = form_data.get("password").unwrap().as_value();
            let reg_attempt = register(&username, &email, &password).await;

            match reg_attempt {
                Ok(att) => {
                    tracing::info!("Registration successful: {:?}", att);
                    req_res.set(att.0);

                    if let Some(_) = att.1 {
                        navigator().push("/");
                    };
                }
                Err(e) => tracing::warn!("Registration failed: {:?}", e),
            }
        });
    };

    rsx! {
        div { class: "min-h-screen flex items-center justify-center",
            div { class: "p-8 bg-base-200 shadow-lg rounded-lg max-w-md w-full",
                h2 { class: "text-center text-2xl font-bold mb-6", "Sign Up" }
                div { class: "flex flex-col space-y-4 mb-6",
                    button {
                        onclick: oauth_call,
                        class: "btn btn-outline btn-accent w-full",
                        "Sign up with GitHub"
                    }
                    button {
                        onclick: google_oauth_call,
                        class: "btn btn-outline btn-accent w-full",
                        "Sign up with Google"
                    }
                }
                div { class: "divider", "OR" }
                form { onsubmit: component_register,
                    div { class: "mb-6",
                        div { class: "label",
                            span { class: "label-text text-sm font-medium block mb-2", "Your email"}
                            if req_res().contains("Email already in use") {
                                span { class: "label-text-alt text-sm font-medium block mb-2 alert-error text-error", "Email already in use"}
                            }
                        }
                        input {
                            r#type: "email",
                            required: "false",
                            name: "email",
                            class: "input input-bordered w-full",
                            class: if req_res().contains("Email already in use") { "input-error" },
                            id: "email"
                        }
                    }
                    div { class: "mb-6",
                        div { class: "label",
                            span { class: "label-text text-sm font-medium block mb-2", "Username"}
                            if req_res().contains("User already exists") {
                                span { class: "label-text-alt text-sm font-medium block mb-2 alert-error text-error", "Username already in use"}
                            }
                        }
                        input {
                            r#type: "text",
                            required: "false",
                            name: "username",
                            class: "input input-bordered w-full",
                            class: if req_res().contains("User already exists") { "input-error" },
                            id: "username"
                        }
                    }
                    div { class: "mb-6",
                        label {
                            r#for: "password",
                            class: "block mb-2 text-sm font-medium",
                            "Password"
                        }
                        input {
                            r#type: "password",
                            required: "false",
                            name: "password",
                            class: "input input-bordered w-full",
                            id: "password"
                        }
                    }
                    div { class: "flex items-center justify-between mb-6",
                        div { class: "flex items-center",
                            input {
                                r#type: "checkbox",
                                class: "checkbox",
                                id: "remember"
                            }
                            label {
                                r#for: "remember",
                                class: "ml-2 text-sm",
                                "Remember me"
                            }
                        }
                    }
                    button { r#type: "submit", class: "btn btn-primary w-full", "Sign up with Email" }
                }
            }
        }
    }
}
