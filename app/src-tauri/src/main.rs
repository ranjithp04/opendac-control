// Hide the console window on Windows release builds (M3).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    opendac_control_app::run();
}
