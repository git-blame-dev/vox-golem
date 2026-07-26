#![forbid(unsafe_code)]
#![deny(unused_must_use)]

#[cfg(feature = "desktop")]
pub mod completion;
#[cfg(feature = "custom-openai")]
pub mod custom_openai;
pub mod inference;
#[cfg(feature = "desktop")]
pub mod llama_cpp;
#[cfg(feature = "desktop")]
pub mod managed_process;
#[cfg(feature = "desktop")]
pub mod opencode;
