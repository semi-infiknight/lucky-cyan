//! Cue system module.
//!
//! The `Cue` trait and all cue implementations live here.  To add a new cue
//! type, implement [`traits::Cue`] + [`traits::CueFactory`], add it to
//! [`types::CueType`], and register the factory in
//! [`registry::CueRegistry`] at startup.

// A property edit is validated by the cue factory before the two instances
// exchange their volatile fields. Swapping also transfers unique player/thread
// handles without restarting them or releasing them with the old cue.
macro_rules! inherit_playback_fields {
    ($($field:ident),+ $(,)?) => {
        fn inherit_playback(&mut self, previous: &mut dyn crate::cue::traits::Cue) {
            let previous = (previous as &mut dyn std::any::Any)
                .downcast_mut::<Self>()
                .expect("property edits preserve the cue type");
            $(std::mem::swap(&mut self.$field, &mut previous.$field);)+
        }
    };
}

pub mod audio_cue;
pub mod camera_cue;
pub mod context;
pub mod control_cue;
pub mod curve;
pub mod decoded_audio;
pub mod devamp_cue;
pub mod fade_cue;
pub mod group_cue;
pub mod light_cue;
pub mod midi_cue;
pub mod msc;
pub mod midi_file_cue;
pub mod mic_cue;
pub mod timecode_cue;
pub mod image_cue;
pub mod media_decode;
pub mod memo_cue;
pub mod osc_cue;
pub mod osc_types;
pub mod point_mix;
pub(crate) mod output_trim;
pub mod registry;
pub mod script_cue;
pub mod stop_cue;
pub mod text_cue;
pub mod traits;
pub mod types;
pub mod validation;
pub mod video_cue;
pub mod wait_cue;

pub use registry::CueRegistry;
pub use traits::Cue;
pub use types::{CueId, CueState, CueType};
