//! Apply a decoded MIDI Show Control message through the native desk transport.

use anyhow::Result;

use crate::cue::{
    msc::MscTransport,
    traits::Cue,
    types::{CueState, CueType},
};

use super::{cue_list::CueList, transport::Transport, Workspace};

pub(super) fn apply(
    transport: &mut Transport,
    show: &mut Workspace,
    command: MscTransport,
    accept_go: impl FnOnce() -> bool,
) -> Result<bool> {
    let command = match command {
        MscTransport::AllOff => {
            let mut changed = false;
            for list in &mut show.cue_lists {
                changed |= list.cues.iter().any(|cue| cue.is_running() || cue.is_paused() || cue.loaded_time_ms().is_some());
                transport.stop_all(list)?;
            }
            return Ok(changed);
        }
        MscTransport::Resume(None) => {
            let mut changed = false;
            for cue in show.cue_lists.iter_mut().flat_map(|list| &mut list.cues) {
                match resume(transport, cue.as_mut()) {
                    Ok(resumed) => changed |= resumed,
                    Err(error) => log::warn!("MIDI Show Control could not resume cue {}: {error}", cue.id()),
                }
            }
            return Ok(changed);
        }
        addressed => addressed,
    };
    let Some(list) = show.active_cue_list_mut() else { return Ok(false) };
    match command {
        MscTransport::Go(number) => {
            let target = if let Some(number) = number.as_deref() {
                list.unique_numbered_cue(number).filter(|cue| !cue.is_disabled())
            } else if list.independent_pads() {
                None
            } else {
                list.playhead_cue()
            };
            let Some(cue) = target else { return Ok(false) };
            if cue.cue_type() == CueType::Audio && cue.file_duration().is_none()
                && cue.media_file_path().is_some_and(|path| !path.as_os_str().is_empty())
            {
                return Ok(false);
            }
            if !list.independent_pads() && !accept_go() {
                return Ok(false);
            }
            let id = cue.id();
            if number.is_some() {
                transport.go_by_id(list, &id)?;
            } else {
                transport.go(list)?;
            }
            Ok(true)
        }
        MscTransport::Standby(forward) => step_standby(list, forward),
        MscTransport::Pause(ref number) | MscTransport::Resume(Some(ref number)) | MscTransport::Load(ref number) => {
            let Some(id) = list.unique_numbered_cue(number).map(|cue| cue.id()) else {
                return Ok(false);
            };
            let Some(cue) = list.get_mut(&id) else { return Ok(false) };
            match command {
                MscTransport::Pause(_) if cue.is_running() => {
                    cue.pause(&transport.context)?;
                    Ok(true)
                }
                MscTransport::Resume(_) => resume(transport, cue),
                MscTransport::Load(_) if cue.state() == CueState::Standby && !cue.is_disabled() => {
                    cue.preload(&transport.context)?;
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
        MscTransport::AllOff | MscTransport::Resume(None) => unreachable!("handled across the desk"),
    }
}

fn resume(transport: &Transport, cue: &mut dyn Cue) -> Result<bool> {
    if !cue.is_paused() || cue.is_disabled() || cue.has_prepared_media() {
        return Ok(false);
    }
    cue.resume(&transport.context)?;
    Ok(true)
}

fn step_standby(list: &mut CueList, forward: bool) -> Result<bool> {
    if list.independent_pads() {
        return Ok(false);
    }
    let current = list.cues.iter().position(|cue| Some(cue.id()) == list.playhead_cue_id);
    let target = if forward {
        let start = current.map_or(0, |index| index + 1);
        list.cues[start..].iter().find(|cue| !cue.is_disabled())
    } else {
        list.cues[..current.unwrap_or(list.cues.len())].iter().rev().find(|cue| !cue.is_disabled())
    };
    let Some(id) = target.map(|cue| cue.id()) else { return Ok(false) };
    list.set_playhead(Some(id))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{audio_cue::AudioCue, types::ContinueMode, wait_cue::WaitCue};
    use std::{sync::Arc, time::Duration};

    #[test]
    fn msc_transport_respects_standby_hold_and_desk_wide_actions() {
        use MscTransport::*;
        let (context, _, calls) = crate::test_support::recording_context();
        let mut transport = Transport::new(context);
        let mut show = Workspace::new("MIDI rehearsal");
        let list = show.active_cue_list_mut().unwrap();
        list.auto_renumber = false;
        let mut make_wait = |number: &str| {
            let mut cue = WaitCue::new();
            cue.set_continue_mode(ContinueMode::DoNotContinue);
            cue.set_number(Some(number.into()));
            let id = cue.id();
            list.push(Box::new(cue));
            id
        };
        let first = make_wait("1.2");
        let second = make_wait("01.2");
        let mut audio = AudioCue::new();
        audio.set_number(Some("6".into()));
        audio.accept_preloaded_audio(Arc::new(vec![0.0; 48_000]), 1, 48_000, Duration::from_secs(1));
        let prepared = audio.id();
        list.push(Box::new(audio));
        list.set_playhead(Some(first)).unwrap();
        let mut auxiliary = CueList::new("Other list");
        let mut wait = WaitCue::new();
        wait.go(&transport.context).unwrap();
        let other = wait.id();
        auxiliary.push(Box::new(wait));
        show.cue_lists.push(auxiliary);

        assert!(apply(&mut transport, &mut show, Standby(true), || panic!("navigation is not GO")).unwrap());
        assert_eq!(show.active_cue_list().unwrap().playhead_cue_id, Some(second));
        assert!(!apply(&mut transport, &mut show, Go(Some("01.2".into())), || false).unwrap());
        assert!(apply(&mut transport, &mut show, Go(Some("01.2".into())), || true).unwrap());
        let list = show.active_cue_list().unwrap();
        assert!(list.get(&second).unwrap().is_running());
        assert!(!list.get(&first).unwrap().is_running());
        assert!(apply(&mut transport, &mut show, Pause("01.2".into()), || true).unwrap());
        assert!(apply(&mut transport, &mut show, Load("6".into()), || true).unwrap());
        assert!(apply(&mut transport, &mut show, Resume(None), || true).unwrap());
        let list = show.active_cue_list().unwrap();
        assert!(list.get(&second).unwrap().is_running());
        assert!(list.get(&prepared).unwrap().has_prepared_media());
        assert_eq!(calls.lock().unwrap().as_slice(), [crate::test_support::EngineCall::AudioPlayPausedRouted { device: None }]);
        assert!(apply(&mut transport, &mut show, AllOff, || true).unwrap());
        assert!(!show.active_cue_list().unwrap().get(&second).unwrap().is_running());
        assert!(!show.cue_lists[1].get(&other).unwrap().is_running());
    }
}
