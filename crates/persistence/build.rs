//! Build script.
//!
//! `sqlx::migrate!()` reads `migrations/` at compile time and embeds it. Cargo
//! has no way to know that, so without this declaration a new migration file
//! leaves the crate's cached build untouched and the application silently
//! ships the previous schema.

fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
