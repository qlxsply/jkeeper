#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app;
mod config;
#[cfg(test)]
mod integration_tests;
mod jar_time;
mod logging;
mod manager;
mod network;
mod port_format;
mod process;
mod scheduler;
slint::include_modules!();
fn main() -> Result<(), Box<dyn std::error::Error>> {
    app::run()
}
