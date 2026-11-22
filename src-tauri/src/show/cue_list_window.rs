//! Operator windows stay bound to one list in one open workspace instance.
use anyhow::{anyhow, ensure, Result};
use uuid::Uuid;

use crate::cue::context::CueContext;
use super::{cue_list::{CueList, CueListMode}, workspace::Workspace};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CueListWindowKind {
    Cart,
    Sequential,
}

impl CueListWindowKind {
    fn prefix(self) -> &'static str {
        match self { Self::Cart => "cart-", Self::Sequential => "cue-list-" }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct CueListWindowTarget {
    instance_id: Uuid,
    pub list_id: Uuid,
    kind: CueListWindowKind,
}

impl CueListWindowTarget {
    pub fn bind(workspace: &Workspace, list_id: Uuid, kind: CueListWindowKind) -> Result<Self> {
        let target = Self {
            instance_id: workspace.instance_id,
            list_id,
            kind,
        };
        target.list(workspace)?;
        Ok(target)
    }

    /// Simple UUIDs avoid ambiguity with the separator and keep labels valid for Tauri.
    pub fn label(self) -> String {
        format!(
            "{}{}-{}",
            self.kind.prefix(),
            self.instance_id.simple(),
            self.list_id.simple()
        )
    }

    pub fn from_label(label: &str, kind: CueListWindowKind) -> Result<Self> {
        let (inst, list) = label
            .strip_prefix(kind.prefix())
            .and_then(|s| s.split_once('-'))
            .ok_or_else(|| anyhow!("This command requires the matching operator window"))?;
        let target = Self {
            instance_id: Uuid::parse_str(inst)?,
            list_id: Uuid::parse_str(list)?,
            kind,
        };
        ensure!(target.label() == label, "Invalid operator window label");
        Ok(target)
    }

    pub fn list(self, ws: &Workspace) -> Result<&CueList> {
        ensure!(
            ws.instance_id == self.instance_id,
            "The original show has closed"
        );
        let list = ws
            .cue_list_by_id(self.list_id)
            .ok_or_else(|| anyhow!("This cue list was removed"))?;
        match self.kind {
            CueListWindowKind::Cart => ensure!(list.independent_pads(), "This window requires independent pads"),
            CueListWindowKind::Sequential => ensure!(list.mode == CueListMode::Sequential, "This window requires a sequential list"),
        }
        Ok(list)
    }

    pub fn standby(self, ws: &Workspace, cue_id: Uuid, child_id: Option<Uuid>) -> Result<Uuid> {
        let list = self.list(ws)?;
        ensure!(
            list.playhead_cue_id == Some(cue_id) && list.enter_standby_child_id() == child_id,
            "Standby moved. Check the new cue before pressing GO."
        );
        let next = child_id.unwrap_or(cue_id);
        ensure!(list.cue_with_arming(&next).is_some_and(|(_, armed)| armed), "Choose an armed standby cue");
        Ok(next)
    }

    pub fn pad_list(self, ws: &mut Workspace, cue_id: Uuid, fire: bool) -> Result<&mut CueList> {
        let list = self.list(ws)?;
        let cue = list
            .get(&cue_id)
            .ok_or_else(|| anyhow!("This pad is no longer in the cart"))?;
        if fire {
            ensure!(!cue.is_disabled(), "This pad is disarmed");
            if let Some(grid) = &list.cart_layout {
                ensure!(
                    grid.pads
                        .get(&cue_id)
                        .copied()
                        .flatten()
                        .is_some_and(|cell| cell.row <= grid.rows && cell.column <= grid.columns),
                    "This pad is outside the visible grid"
                );
            }
        }
        ws.cue_list_by_id_mut(self.list_id)
            .ok_or_else(|| anyhow!("This cart was removed"))
    }
}

pub fn set_list_paused(list: &mut CueList, context: &CueContext, pause: bool) -> Result<()> {
    let mut first_error = None;
    for cue in &mut list.cues {
        let result = if pause && cue.is_running() {
            cue.pause(context)
        } else if !pause && cue.is_paused() && !cue.is_disabled() && !cue.has_prepared_media() {
            cue.resume(context)
        } else {
            continue;
        };
        if first_error.is_none() {
            first_error = result.err();
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{group_cue::GroupCue, traits::Cue, types::GroupMode, wait_cue::WaitCue};

    #[test]
    fn cue_list_window_rejects_stale_workspaces_modes_and_enter_standby() {
        let mut ws = Workspace::new("Rehearsal");
        let list_id = ws.active_cue_list_id;
        let list = ws.active_cue_list_mut().unwrap();
        let mut group = GroupCue::new();
        group.mode = GroupMode::StartFirstAndEnter;
        let first = WaitCue::new();
        let second = WaitCue::new();
        let first_id = first.id();
        let second_id = second.id();
        group.children.extend([Box::new(first) as Box<dyn Cue>, Box::new(second)]);
        let group_id = group.id();
        list.push(Box::new(group));
        list.set_playhead(Some(first_id)).unwrap();

        let kind = CueListWindowKind::Sequential;
        let target = CueListWindowTarget::bind(&ws, list_id, kind).unwrap();
        assert_eq!(CueListWindowTarget::from_label(&target.label(), kind).unwrap(), target);
        assert!(CueListWindowTarget::from_label(&target.label(), CueListWindowKind::Cart).is_err());
        assert_eq!(target.standby(&ws, group_id, Some(first_id)).unwrap(), first_id);
        ws.active_cue_list_mut().unwrap().set_playhead(Some(second_id)).unwrap();
        assert!(target.standby(&ws, group_id, Some(first_id)).is_err());
        assert_eq!(target.standby(&ws, group_id, Some(second_id)).unwrap(), second_id);

        ws.active_cue_list_mut().unwrap().set_mode(CueListMode::Cart, Some(false));
        assert!(target.list(&ws).is_err());
        let mut reopened = Workspace::new("Rehearsal");
        reopened.active_cue_list_mut().unwrap().id = list_id;
        assert!(target.list(&reopened).is_err());
    }

    #[test]
    fn cue_list_window_pause_and_resume_preserve_prepared_voices() {
        use crate::cue::audio_cue::AudioCue;
        use std::{sync::Arc, time::Duration};

        let (context, _, calls) = crate::test_support::recording_context();
        let mut list = CueList::new("Second list");
        let mut wait = WaitCue::new();
        wait.go(&context).unwrap();
        let wait_id = wait.id();
        list.push(Box::new(wait));
        let mut audio = AudioCue::new();
        audio.accept_preloaded_audio(Arc::new(vec![0.0; 4_800]), 1, 48_000, Duration::from_millis(100));
        audio.preload(&context).unwrap();
        let audio_id = audio.id();
        list.push(Box::new(audio));
        set_list_paused(&mut list, &context, true).unwrap();
        assert!(list.get(&wait_id).unwrap().is_paused());
        set_list_paused(&mut list, &context, false).unwrap();
        assert!(list.get(&wait_id).unwrap().is_running());
        assert!(list.get(&audio_id).unwrap().has_prepared_media());
        assert_eq!(calls.lock().unwrap().len(), 1, "Resume must not release the held audio voice");
    }
}
