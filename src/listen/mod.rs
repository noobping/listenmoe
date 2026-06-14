use std::error::Error;

mod clock;
#[cfg(feature = "experimental")]
mod experimental;
#[cfg(not(feature = "experimental"))]
mod stable;
#[cfg(feature = "experimental")]
mod store;
mod stream;
mod viz;

pub use clock::PlaybackClock;
#[cfg(feature = "experimental")]
pub use experimental::Listen;
#[cfg(not(feature = "experimental"))]
pub use stable::Listen;

#[cfg(feature = "experimental")]
pub(in crate::listen) use experimental::Control;
#[cfg(not(feature = "experimental"))]
pub(in crate::listen) use stable::Control;

type DynError = Box<dyn Error + Send + Sync + 'static>;
pub(in crate::listen) type Result<T> = std::result::Result<T, DynError>;

const N_BARS: usize = 48;
