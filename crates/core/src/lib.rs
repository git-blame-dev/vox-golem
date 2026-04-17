#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod config;

pub const APP_NAME: &str = "VoxGolem";

#[cfg(test)]
mod tests {
    use super::APP_NAME;

    #[test]
    fn app_name_stays_stable() {
        assert_eq!(APP_NAME, "VoxGolem");
    }
}
