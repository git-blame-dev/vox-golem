#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod agent_pipeline;
pub mod assistant;
pub mod config;
pub mod conversation;
pub mod conversation_sync;
pub mod durable_conversation;
pub mod external_correlation;
pub mod ipc;
pub mod response_style;
pub mod runtime;
pub mod search;
pub mod session;
#[cfg(feature = "voice-pipeline")]
pub mod turn_capture;
#[cfg(feature = "voice-pipeline")]
pub mod voice_pipeline;
pub mod voice_turn;

pub const APP_NAME: &str = "VoxGolem";

#[cfg(test)]
mod tests {
    use super::APP_NAME;

    #[test]
    fn app_name_stays_stable() {
        assert_eq!(APP_NAME, "VoxGolem");
    }
}
