// FlatBuffer-generierte Typen aus schemas/*.fbs (flatc --rust).
//
// Regenerieren: `make generate` oder:
//   for f in schemas/*.fbs; do flatc --rust -o crates/sentinel-common/src/generated "$f"; done

#[allow(unused_imports, clippy::all, warnings)]
mod action_generated;
#[allow(unused_imports, clippy::all, warnings)]
mod event_generated;
#[allow(unused_imports, clippy::all, warnings)]
mod perception_generated;
#[allow(unused_imports, clippy::all, warnings)]
mod state_generated;

pub use action_generated::sentinel::*;
pub use event_generated::sentinel::*;
pub use perception_generated::sentinel::*;
pub use state_generated::sentinel::*;
