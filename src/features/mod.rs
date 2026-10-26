//! The features of the plugin, one module each. A feature adds its start-up step to
//! [`crate::entry::start`] and, if it hooks the game, its call to the wrappers in `vk`.

pub mod async_compute;
pub mod climb;
pub mod hdr;
pub mod hdr_peak;
pub mod priority;
pub mod scatter_probe;
pub mod texture_budget;
