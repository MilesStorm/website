//! The Enshrouded server panel, for people with the `enshrouded` permission: whether the
//! server is up, who's on, whether a new build is out, and Restart / Force update. The
//! status comes from auth (`services/auth/src/auth/enshrouded.rs`); the page asks again
//! every `POLL_SECS` while the tab is visible.

use dioxus::prelude::*;

use api::enshrouded_command;
#[cfg(target_arch = "wasm32")]
use api::enshrouded_status;
use ui::data_dir::{EnshroudedAction, EnshroudedState, EnshroudedStatus, LoginStatus};

use crate::{LOGIN_STATUS, PERMISSIONS};

/// How often the page asks for the status.
#[cfg(target_arch = "wasm32")]
const POLL_SECS: i64 = 10;

#[component]
pub fn Enshrouded() -> Element {
    let has_perm = PERMISSIONS.read().contains_key("enshrouded");

    match LOGIN_STATUS() {
        LoginStatus::LoggedOut => rsx! {
            div { class: "flex h-screen items-center justify-center",
                p { "Please log in to access the Enshrouded panel." }
            }
        },
        LoginStatus::LoggedIn(_) if !has_perm => rsx! {
            div { class: "flex h-screen items-center justify-center",
                p { "You do not have permission to access the Enshrouded panel." }
            }
        },
        LoginStatus::LoggedIn(_) => rsx! { Panel {} },
    }
}

/// A command waiting for its confirmation, or the answer to the last one.
#[derive(Clone, PartialEq)]
enum CommandUi {
    Idle,
    Confirming(Command),
    Sending(Command),
    Sent(Command),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Command {
    Restart,
    /// Now if nobody is online, else once everyone has left.
    Update,
    /// Disconnects the players.
    UpdateNow,
}

impl Command {
    fn wire(self) -> &'static str {
        match self {
            Command::Restart => "restart",
            Command::Update => "update",
            Command::UpdateNow => "update_now",
        }
    }
}

#[component]
fn Panel() -> Element {
    // Filled in the browser only: the server renders the spinner, as the browser's first frame.
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut status = use_signal(|| None::<Result<EnshroudedStatus, String>>);
    // Unix seconds, ticking every second, for the countdowns and "… ago".
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut now = use_signal(|| 0i64);
    // Set to ask for the status before the next poll (after a command).
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut, unused_variables))]
    let mut refresh = use_signal(|| false);
    let mut command = use_signal(|| CommandUi::Idle);

    use_effect(move || {
        #[cfg(target_arch = "wasm32")]
        spawn(async move {
            let mut next_poll = 0;
            loop {
                let t = (js_sys::Date::now() / 1000.0) as i64;
                now.set(t);
                let hidden = web_sys::window().and_then(|w| w.document()).is_some_and(|d| d.hidden());
                if (t >= next_poll && !hidden) || *refresh.peek() {
                    refresh.set(false);
                    status.set(Some(enshrouded_status().await.map_err(|e| message(&e))));
                    next_poll = t + POLL_SECS;
                }
                gloo_timers::future::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    });

    let send = move |cmd: Command| {
        spawn(async move {
            command.set(CommandUi::Sending(cmd));
            match enshrouded_command(cmd.wire().to_string()).await {
                Ok(()) => command.set(CommandUi::Sent(cmd)),
                Err(e) => command.set(CommandUi::Failed(message(&e))),
            }
            #[cfg(target_arch = "wasm32")]
            refresh.set(true);
        });
    };

    let status = status();
    let now = now();
    rsx! {
        div { class: "container mx-auto max-w-3xl px-4 pt-8 pb-16 flex flex-col gap-6",
            match status {
                None => rsx! {
                    div { class: "flex justify-center py-24",
                        span { class: "loading loading-spinner loading-lg" }
                    }
                },
                Some(Err(e)) => rsx! {
                    div { role: "alert", class: "alert alert-error", "{e}" }
                },
                Some(Ok(s)) => rsx! {
                    Header { status: s.clone() }
                    UpdateNotice { status: s.clone(), now }
                    Players { status: s.clone() }
                    Controls {
                        status: s.clone(),
                        now,
                        command: command(),
                        on_ask: move |cmd| command.set(CommandUi::Confirming(cmd)),
                        on_confirm: send,
                        on_cancel: move |_| command.set(CommandUi::Idle),
                    }
                    Activity { recent: s.recent.clone(), now }
                },
            }
        }
    }
}

/// The server's name, its state, version and uptime.
#[component]
fn Header(status: EnshroudedStatus) -> Element {
    let (badge, label, hint) = state_text(status.state);
    rsx! {
        section { class: "rounded-xl border border-base-300 bg-base-100 px-6 py-6 shadow-sm",
            div { class: "flex flex-wrap items-center justify-between gap-3",
                div {
                    p { class: "text-sm font-semibold uppercase tracking-widest text-accent", "Enshrouded" }
                    h1 { class: "text-3xl font-extrabold", "Chronicle" }
                }
                span { class: "badge badge-lg {badge}",
                    if matches!(status.state, EnshroudedState::Starting | EnshroudedState::Restarting | EnshroudedState::Updating | EnshroudedState::Stopping) {
                        span { class: "loading loading-spinner loading-xs" }
                    }
                    "{label}"
                }
            }
            if !hint.is_empty() {
                p { class: "mt-3 text-sm opacity-70", "{hint}" }
            }
            div { class: "mt-4 flex flex-wrap gap-x-6 gap-y-1 text-sm opacity-70",
                if let Some(up) = status.uptime_secs {
                    span { "Up for {duration(up)}" }
                }
                if let Some(v) = &status.version {
                    span { "Game version {v}" }
                }
                if let Some(b) = &status.build {
                    span { "Steam build {b}" }
                }
            }
        }
    }
}

/// Badge class, label and an explanation for each state.
fn state_text(state: EnshroudedState) -> (&'static str, &'static str, &'static str) {
    match state {
        EnshroudedState::Online => ("badge-success", "Online", ""),
        EnshroudedState::Starting => ("badge-info", "Starting", "Loading the world. Players can join in a minute or two."),
        EnshroudedState::Restarting => ("badge-warning", "Restarting", "Saving the world and starting again."),
        EnshroudedState::Updating => (
            "badge-warning",
            "Updating",
            "Downloading the latest build from Steam. The server starts by itself when it's done; this can take several minutes.",
        ),
        EnshroudedState::Stopping => ("badge-warning", "Stopping", "Saving the world."),
        EnshroudedState::Stopped => ("badge-error", "Stopped", "Restart starts it again."),
        EnshroudedState::Crashed => ("badge-error", "Crashed", "The server kept failing to start. Try Restart or Force update."),
        EnshroudedState::Unreachable => (
            "badge-error",
            "Unreachable",
            "Neither the game nor its control port answers. The server's pod may be starting or moving.",
        ),
    }
}

/// A newer build on Steam than the running server.
#[component]
fn UpdateNotice(status: EnshroudedStatus, now: i64) -> Element {
    if status.update_queued && status.state == EnshroudedState::Online {
        return rsx! {
            div { role: "status", class: "alert alert-info",
                div {
                    p { class: "font-semibold", "An update is queued." }
                    p { class: "text-sm",
                        "It installs once everyone has left the server (checked every 30 minutes). Force update → \"Update now anyway\" installs it right away."
                    }
                }
            }
        };
    }
    let Some(built) = status.latest_build_at else { return rsx! {} };
    let latest = status.latest_build.as_deref().map(|b| format!(" ({b})")).unwrap_or_default();
    if !status.update_pending {
        return rsx! {
            p { class: "px-1 text-sm opacity-60", "Up to date: Steam's latest build{latest} is from {ago(now, built)}." }
        };
    }
    rsx! {
        div { role: "alert", class: "alert alert-info",
            div {
                p { class: "font-semibold", "A new build{latest} came out {ago(now, built)}." }
                p { class: "text-sm",
                    if let Some(b) = &status.build {
                        "The server runs build {b}. "
                    }
                    "It installs the new one by itself when nobody is online (it checks every 30 minutes). Force update installs it now."
                }
            }
        }
    }
}

#[component]
fn Players(status: EnshroudedStatus) -> Element {
    rsx! {
        section { class: "rounded-xl border border-base-300 bg-base-100 px-6 py-5 shadow-sm",
            h2 { class: "text-sm font-semibold uppercase tracking-wide opacity-60", "Players online" }
            match &status.players {
                None => rsx! { p { class: "mt-2 text-2xl font-bold opacity-50", "—" } },
                Some(p) => rsx! {
                    p { class: "mt-2 text-4xl font-extrabold tabular-nums",
                        span { class: if p.online > 0 { "text-success" } else { "" }, "{p.online}" }
                        span { class: "text-xl opacity-50", " / {p.max}" }
                    }
                    if !p.sessions.is_empty() {
                        ul { class: "mt-3 flex flex-col gap-1 text-sm",
                            for (i, secs) in p.sessions.iter().enumerate() {
                                li { class: "flex justify-between gap-4 opacity-80",
                                    span { "Player {i + 1}" }
                                    span { class: "tabular-nums", "on for {duration(*secs as i64)}" }
                                }
                            }
                        }
                        p { class: "mt-2 text-xs opacity-50", "Enshrouded doesn't share player names." }
                    }
                },
            }
        }
    }
}

#[component]
fn Controls(
    status: EnshroudedStatus,
    now: i64,
    command: CommandUi,
    on_ask: EventHandler<Command>,
    on_confirm: EventHandler<Command>,
    on_cancel: EventHandler<()>,
) -> Element {
    let busy = matches!(
        status.state,
        EnshroudedState::Restarting | EnshroudedState::Updating | EnshroudedState::Stopping
    );
    let wait = status.next_command_at.map(|at| at - now).filter(|&s| s > 0);
    let sending = matches!(command, CommandUi::Sending(_));
    let disabled = !status.can_control || busy || wait.is_some() || sending;
    let online = status.players.as_ref().map_or(0, |p| p.online);

    rsx! {
        section { class: "rounded-xl border border-base-300 bg-base-100 px-6 py-5 shadow-sm flex flex-col gap-4",
            h2 { class: "text-sm font-semibold uppercase tracking-wide opacity-60", "Server controls" }
            div { class: "flex flex-wrap gap-3",
                button {
                    class: "btn btn-primary",
                    disabled: disabled,
                    onclick: move |_| on_ask.call(Command::Restart),
                    if command == CommandUi::Sending(Command::Restart) {
                        span { class: "loading loading-spinner loading-sm" }
                    }
                    "Restart"
                }
                button {
                    class: "btn btn-outline btn-secondary",
                    disabled: disabled,
                    onclick: move |_| on_ask.call(Command::Update),
                    if matches!(command, CommandUi::Sending(Command::Update | Command::UpdateNow)) {
                        span { class: "loading loading-spinner loading-sm" }
                    }
                    "Force update"
                }
            }
            if !status.can_control {
                p { class: "text-sm opacity-70", "The website can't reach the server's controls right now." }
            } else if let Some(secs) = wait {
                p { class: "text-sm opacity-70", "Someone just sent a command: the buttons come back in {secs} s." }
            } else if busy {
                p { class: "text-sm opacity-70", "Wait for the current restart or update to finish." }
            }
            match command {
                CommandUi::Confirming(Command::Update) if online > 0 => rsx! {
                    div { role: "alert", class: "alert alert-warning flex flex-col items-start gap-3",
                        p {
                            {players_online(online)}
                            " Force update waits until everyone has left, then installs the latest build (the server checks every 30 minutes)."
                        }
                        p { "\"Update now anyway\" saves and stops the server right away, disconnecting them. Updating takes several minutes." }
                        div { class: "flex flex-wrap gap-2",
                            button { class: "btn btn-sm btn-warning", onclick: move |_| on_confirm.call(Command::Update), "Update when empty" }
                            button { class: "btn btn-sm btn-error btn-outline", onclick: move |_| on_confirm.call(Command::UpdateNow), "Update now anyway" }
                            button { class: "btn btn-sm btn-ghost", onclick: move |_| on_cancel.call(()), "Cancel" }
                        }
                    }
                },
                CommandUi::Confirming(cmd) => rsx! {
                    div { role: "alert", class: "alert alert-warning flex flex-col items-start gap-3",
                        p {
                            match cmd {
                                Command::Restart => "Restart the server? It saves the world, stops, and starts again. Players can rejoin after a minute or two.",
                                Command::Update | Command::UpdateNow => "Force update? The server saves and stops, downloads the latest build from Steam, and starts again. This can take several minutes.",
                            }
                        }
                        if online > 0 {
                            p { class: "font-semibold", {players_online(online)} " They will be disconnected." }
                        }
                        div { class: "flex gap-2",
                            button { class: "btn btn-sm btn-warning", onclick: move |_| on_confirm.call(cmd),
                                match cmd {
                                    Command::Restart => "Yes, restart",
                                    Command::Update | Command::UpdateNow => "Yes, update",
                                }
                            }
                            button { class: "btn btn-sm btn-ghost", onclick: move |_| on_cancel.call(()), "Cancel" }
                        }
                    }
                },
                CommandUi::Sent(cmd) => rsx! {
                    div { role: "status", class: "alert alert-success",
                        match cmd {
                            Command::Restart => "Restart sent. The status above follows along.",
                            Command::Update => "Update requested. It starts now if nobody is online, otherwise once everyone has left.",
                            Command::UpdateNow => "Update started. The status above follows along.",
                        }
                    }
                },
                CommandUi::Failed(e) => rsx! {
                    div { role: "alert", class: "alert alert-error", "{e}" }
                },
                CommandUi::Idle | CommandUi::Sending(_) => rsx! {},
            }
        }
    }
}

/// Who restarted or updated the server lately.
#[component]
fn Activity(recent: Vec<EnshroudedAction>, now: i64) -> Element {
    if recent.is_empty() {
        return rsx! {};
    }
    rsx! {
        section { class: "rounded-xl border border-base-300 bg-base-100 px-6 py-5 shadow-sm",
            h2 { class: "text-sm font-semibold uppercase tracking-wide opacity-60", "Recent commands" }
            ul { class: "mt-3 flex flex-col gap-1 text-sm",
                for a in recent {
                    li { class: "flex justify-between gap-4",
                        span {
                            span { class: "font-semibold", "{a.name.as_deref().unwrap_or(\"A deleted account\")}" }
                            match a.action.as_str() {
                                "update" => " asked for an update",
                                "update_now" => " force-updated the server, disconnecting players",
                                _ => " restarted the server",
                            }
                        }
                        span { class: "opacity-60 whitespace-nowrap", "{ago(now, a.at)}" }
                    }
                }
            }
        }
    }
}

/// "1 player is online." / "3 players are online."
fn players_online(n: u8) -> String {
    if n == 1 { "1 player is online.".to_string() } else { format!("{n} players are online.") }
}

/// The message to show for a failed server function.
fn message(e: &ServerFnError) -> String {
    match e {
        ServerFnError::ServerError { message, .. } => message.clone(),
        _ => "The website can't reach the server right now.".to_string(),
    }
}

/// `secs` as "45 s", "12 min", "3 h 5 min" or "2 d 4 h".
fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    match () {
        _ if d > 0 => format!("{d} d {h} h"),
        _ if h > 0 => format!("{h} h {m} min"),
        _ if m > 0 => format!("{m} min"),
        _ => format!("{secs} s"),
    }
}

/// How long before `now` the moment `at` was (both unix seconds), as "5 min ago".
fn ago(now: i64, at: i64) -> String {
    if now - at < 60 {
        "just now".to_string()
    } else {
        format!("{} ago", duration(now - at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> EnshroudedStatus {
        EnshroudedStatus {
            state: EnshroudedState::Online,
            players: Some(ui::data_dir::EnshroudedPlayers { online: 2, max: 16, sessions: vec![300, 7500] }),
            version: Some("1269579".into()),
            build: Some("25988414".into()),
            latest_build: Some("26000000".into()),
            uptime_secs: Some(90_000),
            latest_build_at: Some(1_000),
            update_pending: true,
            update_queued: false,
            can_control: true,
            recent: vec![
                EnshroudedAction { name: Some("Ada".into()), action: "update_now".into(), at: 9_000 },
                EnshroudedAction { name: None, action: "restart".into(), at: 2_000 },
            ],
            next_command_at: None,
        }
    }

    fn render(status: EnshroudedStatus, command: CommandUi, now: i64) -> String {
        dioxus::ssr::render_element(rsx! { Harness { status, command, now } })
    }

    /// The panel without its polling; the handlers are made here, inside the runtime.
    #[component]
    fn Harness(status: EnshroudedStatus, command: CommandUi, now: i64) -> Element {
        rsx! {
            Header { status: status.clone() }
            UpdateNotice { status: status.clone(), now }
            Players { status: status.clone() }
            Controls {
                status: status.clone(),
                now,
                command,
                on_ask: |_| {},
                on_confirm: |_| {},
                on_cancel: |_| {},
            }
            Activity { recent: status.recent.clone(), now }
        }
    }

    #[test]
    fn the_panel_shows_the_server() {
        let html = render(status(), CommandUi::Idle, 10_000);
        for text in [
            "Online",
            "Up for 1 d 1 h",
            "Game version 1269579",
            "Steam build 25988414",
            "A new build (26000000) came out 2 h 30 min ago",
            "The server runs build 25988414.",
            "on for 5 min",
            "on for 2 h 5 min",
            "Ada",
            " force-updated the server, disconnecting players",
            "A deleted account",
            "16 min ago",
        ] {
            assert!(html.contains(text), "{text:?} in {html}");
        }
        assert!(!html.contains("disabled"), "{html}");
    }

    #[test]
    fn commands_ask_first_and_wait_their_turn() {
        let html = render(status(), CommandUi::Confirming(Command::Restart), 10_000);
        assert!(html.contains("2 players are online. They will be disconnected."), "{html}");
        assert!(html.contains("Yes, restart"), "{html}");

        // With players online, Force update waits for them unless asked not to.
        let html = render(status(), CommandUi::Confirming(Command::Update), 10_000);
        assert!(html.contains("Force update waits until everyone has left"), "{html}");
        assert!(html.contains("Update when empty") && html.contains("Update now anyway"), "{html}");
        let empty = EnshroudedStatus {
            players: Some(ui::data_dir::EnshroudedPlayers { online: 0, max: 16, sessions: vec![] }),
            ..status()
        };
        let html = render(empty, CommandUi::Confirming(Command::Update), 10_000);
        assert!(html.contains("Yes, update") && !html.contains("Update now anyway"), "{html}");

        let current = EnshroudedStatus { update_pending: false, latest_build: Some("25988414".into()), ..status() };
        let html = render(current, CommandUi::Idle, 10_000);
        assert!(html.contains("latest build (25988414) is from 2 h 30 min ago"), "{html}");

        let queued = EnshroudedStatus { update_queued: true, ..status() };
        assert!(render(queued, CommandUi::Idle, 10_000).contains("An update is queued."));

        let waiting = EnshroudedStatus { next_command_at: Some(10_045), ..status() };
        let html = render(waiting, CommandUi::Idle, 10_000);
        assert!(html.contains("the buttons come back in 45 s"), "{html}");
        assert_eq!(html.matches("disabled").count(), 2, "{html}");

        let updating = EnshroudedStatus { state: EnshroudedState::Updating, ..status() };
        let html = render(updating, CommandUi::Idle, 10_000);
        assert!(html.contains("Downloading the latest build"), "{html}");
        assert_eq!(html.matches("disabled").count(), 2, "{html}");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(45), "45 s");
        assert_eq!(duration(-3), "0 s");
        assert_eq!(duration(12 * 60 + 5), "12 min");
        assert_eq!(duration(3 * 3600 + 5 * 60), "3 h 5 min");
        assert_eq!(duration(2 * 86400 + 4 * 3600 + 59), "2 d 4 h");
        assert_eq!(ago(1000, 990), "just now");
        assert_eq!(ago(10_000, 10_000 - 300), "5 min ago");
    }
}
