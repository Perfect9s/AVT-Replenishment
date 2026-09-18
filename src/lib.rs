pub mod core;
pub mod updates;
pub mod xlsx;

#[cfg(windows)]
pub mod self_update;

#[cfg(windows)]
pub mod caption_pin;
