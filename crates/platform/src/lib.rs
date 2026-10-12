#![forbid(unsafe_code)]
#![deny(unused_must_use)]

#[cfg(feature = "desktop")]
pub mod completion;
#[cfg(any(feature = "custom-openai", feature = "custom-openai-webpki"))]
pub mod custom_openai;
pub mod inference;
#[cfg(feature = "desktop")]
pub mod llama_cpp;
#[cfg(feature = "desktop")]
pub mod managed_process;
#[cfg(feature = "desktop")]
pub mod opencode;
#[cfg(any(
    feature = "desktop",
    feature = "custom-openai",
    feature = "custom-openai-webpki"
))]
mod owned_loopback;
#[cfg(feature = "web-search")]
pub mod search;
#[cfg(feature = "web-search")]
pub mod search_control;
#[cfg(feature = "web-search")]
pub mod web;
#[cfg(feature = "desktop")]
pub mod wsl;
