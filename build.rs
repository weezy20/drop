fn main() {
    // Ensure migrations are recompiled when changed
    println!("cargo:rerun-if-changed=migrations");
}
