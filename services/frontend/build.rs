use std::env;

fn main() {
    // run tailwind
    //
    if Ok("debug".to_owned()) == env::var("PROFILE") {
        let _ = std::process::Command::new("npx")
            .args([
                "tailwindcss",
                "-i",
                "input.css",
                "-o",
                "assets/main.css",
                "--minify",
            ])
            .status()
            .expect("failed to run tailwind build");
    }
}
