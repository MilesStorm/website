use std::{cell::RefCell, rc::Rc};

use crate::{
    camera::{self},
    components::Navbar::Navbar,
};
use dioxus::prelude::*;
use web_sys::{
    wasm_bindgen::{closure::Closure, JsCast},
    window, HtmlElement,
};

fn request_animation_frame(f: &Closure<dyn FnMut()>) {
    window()
        .unwrap()
        .request_animation_frame(f.as_ref().unchecked_ref())
        .expect("should register `requestAnimationFrame` OK");
}

#[component()]
pub fn ArcaneEye() -> Element {
    let context = use_resource(move || async move { camera::Canvas::new() });

    rsx! {
        div { class: "bg flex items-center justify-center",
            div { class: "container mx-auto text-center  flex flex-col items-center justify-center",
                h1 { class:"text-3xl font-bold mb-8", "ArcaneEye" }
                canvas { class: "rounded", id: "pre", onmounted: move |_| {
                let web_cam = camera::WebCam::new();

                let _pre = window()
                    .unwrap()
                    .document()
                    .unwrap()
                    .get_element_by_id("pre")
                    .unwrap()
                    .dyn_into::<HtmlElement>()
                    .unwrap();

                    spawn(async move {
                        if let Ok(()) = &web_cam.setup().await {
                            let f = Rc::new(RefCell::new(None));
                            let g = f.clone();

                            *g.borrow_mut() = Some(Closure::new(move || {
                                request_animation_frame(f.borrow().as_ref().unwrap());
                                match &(*context.peek()) {
                                    Some(ctx) => {
                                        ctx.draw_image(&web_cam.video);
                                    }
                                    None => {
                                        tracing::error!("Failed to get context");
                                        return;
                                    }
                                }
                            }));
                            request_animation_frame(g.borrow().as_ref().unwrap());
                        } else {
                            tracing::error!("Failed to setup camera");
                        }
                    });
                }}
                button { class: "btn btn-primary mt-4", onclick: move |_| {
                    match &(*context.peek()) {
                        Some(ctx) => {
                            tracing::info!("Capturing image");
                            ctx.get_image_data();
                        }
                        None => {
                            tracing::error!("Failed to get context");
                            return;
                        }
                    }
                }, "Capture"}
            }
        }
    }
}

#[component]
pub fn ArcaneEyePage() -> Element {
    rsx! {
        div {
            Navbar {}
            ArcaneEye {}
        }
    }
}
