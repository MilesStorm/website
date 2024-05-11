use dioxus::prelude::*;

use crate::components::login::LoginWidget;
#[component]
pub fn Login() -> Element {
    LoginWidget()
}

#[component]
pub fn Register(segments: Vec<String>) -> Element {
    rsx! {
        div { class: "max-w-md w-full space-y-8 p-10 bg-white rounded-lg shadow-lg text-black",
            h2 { class: "mt-6 text-center text-3xl font-extrabold text-gray-900", "Create your account" }
            form { action: "#", method: "POST", class: "mt-8 space-y-6",
                input { name: "remember", value: "true", r#type: "hidden" }
                div { class: "rounded-md shadow-sm -space-y-px",
                    div {
                        label { r#for: "full-name", class: "sr-only", "Full name" }
                        input {
                            autocomplete: "name",
                            required: "false",
                            name: "full-name",
                            r#type: "text",
                            placeholder: "Full name",
                            class: "appearance-none rounded-none relative block w-full px-3 py-2 border border-gray-300 placeholder-gray-500 text-gray-900 rounded-t-md focus:outline-none focus:ring-indigo-500 focus:border-indigo-500 focus:z-10 sm:text-sm",
                            id: "full-name"
                        }
                    }
                    div {
                        label { r#for: "email-address", class: "sr-only", "Email address" }
                        input {
                            autocomplete: "email",
                            r#type: "email",
                            placeholder: "Email address",
                            required: "false",
                            name: "email",
                            class: "appearance-none rounded-none relative block w-full px-3 py-2 border border-gray-300 placeholder-gray-500 text-gray-900 focus:outline-none focus:ring-indigo-500 focus:border-indigo-500 sm:text-sm",
                            id: "email-address"
                        }
                    }
                    div {
                        label { r#for: "password", class: "sr-only", "Password" }
                        input {
                            name: "password",
                            required: "false",
                            autocomplete: "new-password",
                            placeholder: "Password",
                            r#type: "password",
                            class: "appearance-none rounded-none relative block w-full px-3 py-2 border border-gray-300 placeholder-gray-500 text-gray-900 rounded-b-md focus:outline-none focus:ring-indigo-500 focus:border-indigo-500 sm:text-sm",
                            id: "password"
                        }
                    }
                }
                div { class: "flex items-center justify-between",
                    div { class: "flex items-center",
                        input {
                            name: "agree",
                            r#type: "checkbox",
                            class: "h-4 w-4 text-indigo-600 focus:ring-indigo-500 border-gray-300 rounded",
                            id: "agree"
                        }
                        label {
                            r#for: "agree",
                            class: "ml-2 block text-sm text-gray-900",
                            "\n          Agree to "
                            a {
                                href: "#",
                                class: "font-medium text-indigo-600 hover:text-indigo-500",
                                "terms and conditions"
                            }
                        }
                    }
                }
                div {
                    button {
                        r#type: "submit",
                        class: "group relative w-full flex justify-center py-2 px-4 border border-transparent text-sm font-medium rounded-md text-white bg-indigo-600 hover:bg-indigo-700 focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-indigo-500",
                        "\n        Sign Up\n      "
                    }
                }
            }
        }
    }
}
