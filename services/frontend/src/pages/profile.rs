use dioxus::prelude::*;

use crate::{
    components::Navbar::Navbar,
    get_mode,
    pages::{set_mode, Theme},
};

const PROFILE_PIC: manganis::ImageAsset = manganis::mg!(image("assets/default_profile.png"));

#[component]
pub fn Profile() -> Element {
    rsx! {
        Navbar {}
        Profile_form {}
    }
}

#[component]
pub fn Profile_form() -> Element {
    rsx! {
        div { class: "container mx-auto mt-10",
            div { class: "bg-base-200 p-10 rounded-lg shadow-lg max-w-4xl mx-auto",
                h1 { class: "text-2xl font-bold mb-10", "Profile Account" }
                div { class: "grid grid-cols-1 md:grid-cols-2 gap-x-10 gap-y-6",
                    div { class: "mb-4",
                        label { class: "block  text-sm font-bold mb-2", "Profile Photo" }
                        img { class:"w-24 h-24 rounded-full mb-4", src: "{PROFILE_PIC}", alt: "profile picture"}
                        input { r#type: "file", class: "file-input file-input-primary w-full max-w-xs" }
                    }
                    div { class: "mb-4",
                        label { class: "block  text-sm font-bold mb-2", "Username" }
                        input {
                            r#type: "text",
                            placeholder: "Username",
                            class: "input input-primary w-full max-w-xs"
                        }
                    }
                    div { class: "mb-4",
                        label { class: "block  text-sm font-bold mb-2", "Email" }
                        input {
                            r#type: "email",
                            placeholder: "Email",
                            class: "input input-primary w-full max-w-xs"
                        }
                        // p { class: "text-sm  mt-1",
                        //     "Please check your email to verify your account. "
                        //     a { href: "#", class: "text-blue-500", "Resend" }
                        // }
                    }
                    div { class: "mb-4",
                        label { class: "block  text-sm font-bold mb-2", "Site Theme" }
                        select { class: "select select-primary w-full max-w-xs",
                            onchange: move |evt: Event<FormData>| {
                                let choise = evt.value();
                                let new_theme = Theme::from(choise);
                                set_mode(new_theme);
                            },
                            value: {
                                match get_mode() {
                                    Theme::Dark=>"Dark",
                                    Theme::Light=>"Light",
                                    Theme::Preffered=>"System",
                                    Theme::Dracula => "Dracula",
                                    Theme::Synthwave => "Synthwave",
                                    Theme::Retro => "Retro",
                                    Theme::Dim => "Dim",
                                    Theme::Corporate => "Corporate",
                                }
                            },
                            for theme in Theme::iterator() {
                                 option { "{theme}" }
                            }
                        }
                    }
                    div { class: "mb-4",
                        label { class: "block  text-sm font-bold mb-2", "Full Name" }
                        input {
                            placeholder: "Full name",
                            r#type: "text",
                            class: "input input-primary w-full max-w-xs"
                        }
                    }
                    div { class: "mb-4",
                        label { class: "block text-sm font-bold mb-2", "Language" }
                        select { class: "select select-primary w-full max-w-xs",
                            option { "English" }
                            option { "Spanish" }
                        }
                    }
                }
                div { class: "flex justify-end mt-10",
                    button { class: "btn bg-purple-500 hover:bg-purple-700 text-white font-bold py-2 px-4 rounded",
                        "Update"
                    }
                }
                div { class: "mt-10",
                    h2 { class: "text-xl font-bold mb-2", "Delete Account" }
                    div { class: "mb-4",
                        input {
                            placeholder: "Confirm your Email",
                            r#type: "email",
                            class: "input input-primary w-full max-w-xs"
                        }
                    }
                    button { class: "btn bg-red-500 hover:bg-red-700 text-white font-bold py-2 px-4 rounded",
                        "Delete Account"
                    }
                }
            }
        }
    }
}
