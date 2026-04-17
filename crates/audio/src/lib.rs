#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub const AUDIO_MODULE_READY: bool = true;

#[cfg(test)]
mod tests {
    use super::AUDIO_MODULE_READY;

    #[test]
    fn module_flag_defaults_to_ready() {
        assert!(AUDIO_MODULE_READY);
    }
}
