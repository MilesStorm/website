// The roll-sharing schema is compiled in by `surrealkit::embed_schema!` (src/dataset.rs).
// Editing a file there already triggers a rebuild; this also catches added or removed ones.
fn main() {
    println!("cargo:rerun-if-changed=../../surreal/database/schema");
}
