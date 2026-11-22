//! Show layer — workspace, cue list, transport, and background event loop.

pub mod cart_layout;
pub mod cue_list_window;
pub mod crossfade;
pub(crate) mod cue_colors;
pub mod cue_list;
pub mod event_loop;
mod msc_transport;
pub(crate) mod timecode_loss;
pub mod transport;
pub mod undo_stack;
pub mod wall_clock;
pub mod workspace;

pub use workspace::Workspace;
