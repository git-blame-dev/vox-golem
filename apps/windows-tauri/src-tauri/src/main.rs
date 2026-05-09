#![forbid(unsafe_code)]
#![deny(unused_must_use)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    vox_golem_windows_lib::run();
}
