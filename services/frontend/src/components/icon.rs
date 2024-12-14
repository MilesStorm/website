use dioxus::prelude::*;

#[derive(PartialEq, Props, Clone)]
pub struct IconProps {
    width: u32,
    height: u32,
    class: Option<String>,
}

const DEFAULT_PROFILE: Asset = manganis::asset!("assets/default_profile.png");

pub fn Logo_c(props: IconProps) -> Element {
    rsx! {
        svg {
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 300 300",
            width: "{props.width}px",
            height: "{props.height}px",
            class: props.class.unwrap_or("".into()),
            path {
                class: "fill-slate-700 dark:fill-slate-200 fill-ico ",
                stroke: "#303841",
                stroke_width: ".56",
                d: "M2.17-100.5 35.7-19.51l35.63-52.33-69.17-28.64zm-3.44 0-69.17 28.66 35.57 52.48 33.6-81.13zm1.72 2.13-33.7 81.39L.27 32.5 34.1-17.15.45-98.37zM73-70 36.81-16.87l23.85 57.61L101.68-.28l.34.34L73-70zm-145.08 0L-101.12.08l.33-.33 41.01 41.02 23.8-57.47-36.1-53.3zM35.19-14.5 1.73 34.64 28.01 73.4l30.82-30.82L35.19-14.5zm-69.55.17L-57.94 42.6l30.54 30.56 26.23-38.52-33.19-48.97zm135.19 18.3L61.66 43.14l11.47 27.7 27.7-66.87zM-99.91 4l27.68 66.82 11.46-27.67L-99.91 4zM.28 36.77l-25.95 38.11L.43 101l25.85-25.86-26-38.36zm59.54 8.21L29.38 75.43l7.9 11.67 34.13-14.14-11.59-27.98zM-58.93 45l-11.59 27.96 33.74 13.97 8-11.75L-58.92 45zm31.88 31.9-7.47 10.97 31.46 13.03-23.99-24zm54.7.26L3.87 100.94l31.15-12.9-7.37-10.88z",
                style: "-inkscape-stroke:none;",
                transform: "matrix(1.5 0 0 1.5 149.34 149.15)"
            }
        }
    }
}

pub fn default_profile_picture(props: IconProps) -> Element {
    rsx! {
        img {
            src: "{DEFAULT_PROFILE}",
            alt: "profile picture",
            width: "{props.width}",
            height: "{props.height}",
            class: "rounded-full"
        }
    }
}
