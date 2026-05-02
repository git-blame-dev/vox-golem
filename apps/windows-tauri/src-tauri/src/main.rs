#![forbid(unsafe_code)]
#![deny(unused_must_use)]
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

fn main() {
    vox_golem_windows_lib::run();
}
