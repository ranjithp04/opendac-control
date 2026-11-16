fn main() {
    // `tauri::generate_context!` needs the frontend dist directory to exist.
    // Create it empty when the UI has not been built, so `cargo build|clippy
    // --workspace` works without Node. `npm run build` in app/ui fills it.
    let dist = std::path::Path::new("../ui/dist");
    if !dist.exists() {
        std::fs::create_dir_all(dist).expect("create ../ui/dist");
    }
    tauri_build::build()
}
