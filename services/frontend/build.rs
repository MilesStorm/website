use std::env;

fn main() {
    if Ok("debug".to_owned()) == env::var("PROFILE") {
        let _ = std::process::Command::new("npx")
            .args([
                "tailwindcss",
                "-i",
                "tailwind.css",
                "-o",
                "assets/tailwind.css",
                "--minify",
            ])
            .status()
            .expect("failed to run tailwind build");
    }
}
