//! Apply a decoded MIDI Show Control message through the native desk transport.

use anyhow::Result;

use crate::cue::{
    msc::{MscAction, MscTransport},
    traits::Cue,
    types::{CueState, CueType},
};
use crate::engine::midi_transport::MidiTransportAction;

use super::{cue_list::CueList, cue_list_window::set_list_paused, transport::Transport, Workspace};

pub(super) fn apply_workspace_binding(
    transport: &mut Transport, show: &mut Workspace, action: MidiTransportAction, accept_go: impl FnOnce() -> bool,
) -> Result<bool> {
    use MidiTransportAction::*;
    let action = match action {
        Go => MscAction::Go(None),
        StopAll => MscAction::AllOff,
        ResumeAll => MscAction::Resume(None),
        NextStandby => MscAction::Step { forward: true, sequence: false },
        PreviousStandby => MscAction::Step { forward: false, sequence: false },
        NextSequence => MscAction::Step { forward: true, sequence: true },
        PreviousSequence => MscAction::Step { forward: false, sequence: true },
        PauseAll => {
            let mut first_error = None;
            for list in &mut show.cue_lists {
                let result = set_list_paused(list, &transport.context, true);
                if first_error.is_none() { first_error = result.err(); }
            }
            return first_error.map_or(Ok(true), Err);
        }
        HardStop => {
            for list in &mut show.cue_lists { transport.hard_stop_all(list)?; }
            return Ok(true);
        }
    };
    apply(transport, show, MscTransport { action, list: None }, accept_go)
}

pub(super) fn apply(
    transport: &mut Transport,
    show: &mut Workspace,
    command: MscTransport,
    accept_go: impl FnOnce() -> bool,
) -> Result<bool> {
    let MscTransport { action, list: list_number } = command;
    let action = match action {
        MscAction::AllOff => {
            let mut changed = false;
            for list in &mut show.cue_lists {
                changed |= list.cues.iter().any(|cue| cue.is_running() || cue.is_paused() || cue.loaded_time_ms().is_some());
                transport.stop_all(list)?;
            }
            return Ok(changed);
        }
        MscAction::Reset(None) => {
            transport.reset_workspace(&mut show.cue_lists)?;
            return Ok(true);
        }
        MscAction::Resume(None) => {
            let mut changed = false;
            for cue in show.cue_lists.iter_mut().flat_map(|list| &mut list.cues) {
                match resume(transport, cue.as_mut()) {
                    Ok(resumed) => changed |= resumed,
                    Err(error) => log::warn!("MIDI Show Control could not resume cue {}: {error}", cue.id()),
                }
            }
            return Ok(changed);
        }
        MscAction::Pause(None) | MscAction::Load(None) => {
            let Some(list) = show.cue_lists.iter_mut().find(|list| list.id == show.active_cue_list_id) else {
                return Ok(false);
            };
            let mut changed = false;
            for id in &show.desk_selection {
                let Some(cue) = list.get_mut_recursive(id) else { continue };
                match apply_cue_action(transport, &action, cue) {
                    Ok(applied) => changed |= applied,
                    Err(error) => log::warn!("MIDI Show Control could not act on selected cue {id}: {error}"),
                }
            }
            return Ok(changed);
        }
        addressed => addressed,
    };
    let list = if let Some(number) = list_number.as_deref() {
        let mut matching = show.cue_lists.iter_mut().filter(|list| list.number.as_deref() == Some(number));
        matching.next().filter(|_| matching.next().is_none())
    } else {
        show.active_cue_list_mut()
    };
    let Some(list) = list else { return Ok(false) };
    match action {
        MscAction::Go(number) => {
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
        MscAction::Step { forward, sequence } => list.step_standby(forward, sequence),
        MscAction::Reset(Some(number)) => {
            let Some(id) = list.unique_numbered_cue(&number).map(|cue| cue.id()) else {
                return Ok(false);
            };
            transport.reset_cue(list, id)?;
            Ok(true)
        }
        MscAction::Pause(Some(ref number)) | MscAction::Resume(Some(ref number)) | MscAction::Load(Some(ref number)) => {
            let Some(id) = list.unique_numbered_cue(number).map(|cue| cue.id()) else {
                return Ok(false);
            };
            let Some(cue) = list.get_mut(&id) else { return Ok(false) };
            apply_cue_action(transport, &action, cue)
        }
        MscAction::AllOff | MscAction::Resume(None) | MscAction::Pause(None) | MscAction::Load(None) | MscAction::Reset(None) => {
            unreachable!("handled before selecting a numbered target")
        }
    }
}

fn apply_cue_action(transport: &Transport, action: &MscAction, cue: &mut dyn Cue) -> Result<bool> {
    match action {
        MscAction::Pause(_) if cue.is_running() => {
            cue.pause(&transport.context)?;
            Ok(true)
        }
        MscAction::Resume(_) => resume(transport, cue),
        MscAction::Load(_) if cue.state() == CueState::Standby && !cue.is_disabled() => {
            cue.preload(&transport.context)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn resume(transport: &Transport, cue: &mut dyn Cue) -> Result<bool> {
    if !cue.is_paused() || cue.is_disabled() || cue.has_prepared_media() {
        return Ok(false);
    }
    cue.resume(&transport.context)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{audio_cue::AudioCue, types::ContinueMode, wait_cue::WaitCue};
    use std::{sync::Arc, time::Duration};

    #[test]
    fn workspace_midi_controls_match_one_transport_action_and_survive_recovery() {
        use crate::engine::{
            midi_transport::{MidiTransportBinding, MidiValueMatch},
            midi_trigger::{MidiTrigger, MidiTriggerType},
        };
        use MidiTransportAction::*;
        let (context, _, _) = crate::test_support::recording_context();
        let mut transport = Transport::new(context);
        let mut show = Workspace::new("MIDI rehearsal");
        let mut wait = WaitCue::new();
        wait.set_continue_mode(ContinueMode::DoNotContinue);
        let id = wait.id();
        show.active_cue_list_mut().unwrap().push(Box::new(wait));
        let go = MidiTransportBinding {
            trigger: MidiTrigger { message_type: MidiTriggerType::ControlChange, channel: 0, data1: 64, data2: Some(64) },
            value_match: MidiValueMatch::Above,
        };
        show.midi_controls.bindings.insert(Go, go);
        assert_eq!(show.midi_controls.action(&[0xb0, 64, 127]), None);
        show.midi_controls.enabled = true;
        assert_eq!(show.midi_controls.action(&[0xb0, 64, 0]), None);
        let mut pause = go;
        pause.trigger.channel = 1;
        show.midi_controls.bindings.insert(PauseAll, pause);
        assert!(show.midi_controls.validate().is_err());
        pause.trigger.data2 = Some(1);
        pause.value_match = MidiValueMatch::Below;
        show.midi_controls.bindings.insert(PauseAll, pause);
        show.midi_controls.validate().unwrap();
        assert_eq!(show.midi_controls.action(&[0xb0, 64]), None);
        let action = show.midi_controls.action(&[0xb0, 64, 127]).unwrap();
        assert_eq!(action, Go);
        assert!(apply_workspace_binding(&mut transport, &mut show, action, || true).unwrap());
        let action = show.midi_controls.action(&[0xb0, 64, 0]).unwrap();
        assert_eq!(action, PauseAll);
        assert!(apply_workspace_binding(&mut transport, &mut show, action, || false).unwrap());
        assert!(show.active_cue_list().unwrap().get(&id).unwrap().is_paused());
        let recovered = Workspace::from_json_str(&show.to_recovery_json().unwrap(), None,
            &crate::test_support::full_registry()).unwrap();
        assert_eq!(recovered.midi_controls, show.midi_controls);
    }

    #[test]
    fn scoped_msc_moves_sequences_and_runs_another_list_without_selecting_it() {
        let (context, _, _) = crate::test_support::recording_context();
        let mut transport = Transport::new(context);
        let mut show = Workspace::new("MIDI rehearsal");
        let active_id = show.active_cue_list_id;
        let mut cue = WaitCue::new();
        cue.set_number(Some("42".into()));
        let active_standby = cue.id();
        let active = show.active_cue_list_mut().unwrap();
        active.number = Some("7".into());
        active.push(Box::new(cue));
        let mut secondary = CueList::new("Secondary");
        secondary.number = Some("07".into());
        let ids: Vec<_> = [
            ("5", ContinueMode::AutoContinue),
            ("6", ContinueMode::DoNotContinue),
            ("20", ContinueMode::AutoFollow),
            ("21", ContinueMode::DoNotContinue),
            ("42", ContinueMode::DoNotContinue),
        ].into_iter().map(|(number, mode)| {
            let mut cue = WaitCue::new();
            cue.set_number(Some(number.into()));
            cue.set_continue_mode(mode);
            let id = cue.id();
            secondary.push(Box::new(cue));
            id
        }).collect();
        show.cue_lists.push(secondary);
        let send = |transport: &mut Transport, show: &mut Workspace, command, operand: &[u8]| {
            let mut bytes = vec![0xf0, 0x7f, 7, 2, 16, command];
            bytes.extend_from_slice(operand);
            bytes.push(0xf7);
            apply(transport, show, MscTransport::decode(&bytes, 7).unwrap(), || true).unwrap()
        };

        assert!(send(&mut transport, &mut show, 19, b"07"));
        assert_eq!(show.cue_lists[1].playhead_cue_id, Some(ids[2]));
        assert!(send(&mut transport, &mut show, 17, b"07"));
        assert_eq!(show.cue_lists[1].playhead_cue_id, Some(ids[3]));
        assert!(send(&mut transport, &mut show, 20, b"07"));
        assert_eq!(show.cue_lists[1].playhead_cue_id, Some(ids[0]));
        assert!(show.cue_lists[1].cues.iter().all(|cue| cue.state() == CueState::Standby));
        assert!(send(&mut transport, &mut show, 1, b"42\x0007"));
        assert_eq!(show.active_cue_list_id, active_id);
        assert_eq!(show.active_cue_list().unwrap().playhead_cue_id, Some(active_standby));
        assert!(!show.active_cue_list().unwrap().get(&active_standby).unwrap().is_running());
        assert!(show.cue_lists[1].get(&ids[4]).unwrap().is_running());
        show.active_cue_list_mut().unwrap().number = Some("07".into());
        assert!(!send(&mut transport, &mut show, 2, b"42\x0007"));
        assert!(show.cue_lists[1].get(&ids[4]).unwrap().is_running());
    }

    #[test]
    fn msc_reset_stops_the_addressed_cue_or_rewinds_all_lists() {
        let (context, _, calls) = crate::test_support::recording_context();
        let mut transport = Transport::new(context);
        let mut show = Workspace::new("Reset rehearsal");
        let make_list = |list: &mut CueList, number: &str| {
            let mut audio = AudioCue::new();
            audio.accept_preloaded_audio(Arc::new(vec![0.0; 4800]), 1, 48_000, Duration::from_millis(100));
            let first = audio.id();
            list.push(Box::new(audio));
            let mut wait = WaitCue::new();
            wait.set_number(Some(number.into()));
            wait.go(&transport.context).unwrap();
            let running = wait.id();
            list.push(Box::new(wait));
            list.set_playhead(Some(running)).unwrap();
            (first, running)
        };
        let (first, running) = make_list(show.active_cue_list_mut().unwrap(), "04");
        let mut secondary = CueList::new("Secondary");
        let (second_first, second_running) = make_list(&mut secondary, "04");
        show.cue_lists.push(secondary);

        let reset = |target: Option<&str>| MscTransport { action: MscAction::Reset(target.map(str::to_owned)), list: None };
        assert!(apply(&mut transport, &mut show, reset(Some("04")), || false).unwrap());
        let list = show.active_cue_list().unwrap();
        assert_eq!(list.playhead_cue_id, Some(running));
        assert_eq!(list.get(&running).unwrap().state(), CueState::Standby);
        assert!(show.cue_lists[1].get(&second_running).unwrap().is_running());

        assert!(apply(&mut transport, &mut show, reset(None), || false).unwrap());
        let list = show.active_cue_list().unwrap();
        assert_eq!(list.playhead_cue_id, Some(first));
        assert!(list.get(&first).unwrap().has_prepared_media());
        assert_eq!(show.cue_lists[1].playhead_cue_id, Some(second_first));
        assert!(show.cue_lists[1].get(&second_first).unwrap().has_prepared_media());
        assert_eq!(show.cue_lists[1].get(&second_running).unwrap().state(), CueState::Standby);
        let calls = calls.lock().unwrap();
        let prepared = calls.iter().filter(|call| matches!(call, crate::test_support::EngineCall::AudioPlayPausedRouted { .. })).count();
        assert_eq!(prepared, 2);
    }

    #[test]
    fn msc_transport_respects_standby_hold_and_desk_wide_actions() {
        use MscAction::*;
        let unscoped = |action| MscTransport { action, list: None };
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

        assert!(apply(&mut transport, &mut show, unscoped(Step { forward: true, sequence: false }),
            || panic!("navigation is not GO")).unwrap());
        assert_eq!(show.active_cue_list().unwrap().playhead_cue_id, Some(second));
        assert!(!apply(&mut transport, &mut show, unscoped(Go(Some("01.2".into()))), || false).unwrap());
        assert!(apply(&mut transport, &mut show, unscoped(Go(Some("01.2".into()))), || true).unwrap());
        let list = show.active_cue_list().unwrap();
        assert!(list.get(&second).unwrap().is_running());
        assert!(!list.get(&first).unwrap().is_running());
        assert!(!apply(&mut transport, &mut show, unscoped(Pause(None)), || true).unwrap());
        let standby = show.active_cue_list().unwrap().playhead_cue_id;
        show.desk_selection = vec![second, other, prepared];
        assert!(apply(&mut transport, &mut show, unscoped(Pause(None)), || true).unwrap());
        assert!(show.active_cue_list().unwrap().get(&second).unwrap().is_paused());
        assert!(show.cue_lists[1].get(&other).unwrap().is_running());
        assert!(apply(&mut transport, &mut show, unscoped(Load(None)), || true).unwrap());
        assert_eq!(show.active_cue_list().unwrap().playhead_cue_id, standby);
        let active_id = show.active_cue_list_id;
        let other_id = show.cue_lists[1].id;
        show.switch_active_list(other_id);
        show.switch_active_list(active_id);
        assert!(show.desk_selection.is_empty());
        assert!(apply(&mut transport, &mut show, unscoped(Resume(None)), || true).unwrap());
        let list = show.active_cue_list().unwrap();
        assert!(list.get(&second).unwrap().is_running());
        assert!(list.get(&prepared).unwrap().has_prepared_media());
        assert_eq!(calls.lock().unwrap().as_slice(), [crate::test_support::EngineCall::AudioPlayPausedRouted { device: None }]);
        assert!(apply(&mut transport, &mut show, unscoped(AllOff), || true).unwrap());
        assert!(!show.active_cue_list().unwrap().get(&second).unwrap().is_running());
        assert!(!show.cue_lists[1].get(&other).unwrap().is_running());
    }
}
