mod about;
mod debug;
mod devices;
mod logs;
mod new_version_popup;
mod notifications;
mod settings;
mod settings_controls;
mod setup_wizard;
mod statistics;

#[cfg(not(target_arch = "wasm32"))]
mod diagnostics;
#[cfg(not(target_arch = "wasm32"))]
mod installation;
#[cfg(not(target_arch = "wasm32"))]
mod latency_lab;

pub use about::*;
pub use debug::*;
pub use devices::*;
pub use logs::*;
pub use new_version_popup::*;
pub use notifications::*;
pub use settings::*;
pub use settings_controls::*;
pub use setup_wizard::*;
pub use statistics::*;

#[cfg(not(target_arch = "wasm32"))]
pub use diagnostics::*;
#[cfg(not(target_arch = "wasm32"))]
pub use installation::*;
#[cfg(not(target_arch = "wasm32"))]
pub use latency_lab::*;
