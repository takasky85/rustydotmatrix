// NOTE: the console window is deliberately left visible on Windows while we debug startup problems.
// Re-add `#![windows_subsystem = "windows"]` once things are stable.

mod app;

fn main() {
    app::run();
}
