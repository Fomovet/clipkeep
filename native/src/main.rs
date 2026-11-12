// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    clipkeep_lib::run()
}
