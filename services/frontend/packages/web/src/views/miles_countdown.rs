use dioxus::prelude::*;

#[component]
pub fn AssholeTimer() -> Element {
    let mut remaining = use_signal(|| None::<(u64, u64, u64, u64)>);

    use_effect(move || {
        #[cfg(target_arch = "wasm32")]
        spawn(async move {
            use rand::Rng;
            let mut rng = rand::rng();

            let days = rng.random_range(1u64..=365);
            if days < 30 {
                if let Some(w) = web_sys::window() {
                    let _ = w.location().reload();
                }
                return;
            }

            let extra_secs = rng.random_range(0u64..86400);
            let now_ms = js_sys::Date::now() as u64;
            let target_ms = now_ms + (days * 86400 + extra_secs) * 1000;

            loop {
                let now = js_sys::Date::now() as u64;
                if now >= target_ms {
                    break;
                }
                let secs = (target_ms - now) / 1000;
                remaining.set(Some((
                    secs / 86400,
                    (secs % 86400) / 3600,
                    (secs % 3600) / 60,
                    secs % 60,
                )));
                gloo_timers::future::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    });

    rsx! {
        div { class: "flex flex-col items-center justify-center min-h-[60vh] gap-4",
            h1 { class: "text-2xl font-bold", "⏳ Time until Miles stops being an asshole" }
            match remaining() {
                None => rsx! {
                    p { class: "text-base-content/50 text-sm", "calculating..." }
                },
                Some((d, h, m, s)) => rsx! {
                    p { class: "text-4xl font-mono tabular-nums",
                        "{d}d {h}h {m}m {s}s"
                    }
                }
            }
        }
    }
}
