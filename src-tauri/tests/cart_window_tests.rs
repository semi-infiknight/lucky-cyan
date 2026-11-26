//! Secondary carts address a particular open show and exact top-level pad.
mod common;

use inkue_lib::cue::{traits::Cue, types::ContinueMode, wait_cue::WaitCue};
use inkue_lib::show::{
    cart_layout::{CartCell, CartEdit},
    cue_list::{CueList, CueListMode},
    cue_list_window::{CueListWindowKind, CueListWindowTarget},
    transport::Transport,
    Workspace,
};

fn bind_cart(ws: &Workspace, list_id: uuid::Uuid) -> CueListWindowTarget {
    CueListWindowTarget::bind(ws, list_id, CueListWindowKind::Cart).unwrap()
}

fn wait_pad(number: &str) -> Box<dyn Cue> {
    let mut pad = WaitCue::new();
    pad.set_number(Some(number.into()));
    pad.set_continue_mode(ContinueMode::AutoContinue);
    Box::new(pad)
}

#[test]
fn a_bound_pad_fires_exactly_one_uuid_and_preserves_main_standby() {
    let mut ws = Workspace::new("Performance");
    let standby = wait_pad("10");
    let standby_id = standby.id();
    ws.active_cue_list_mut().unwrap().push(standby);
    let main_list = ws.active_cue_list_id;
    let mut cart = CueList::new("Thunder");
    cart.set_mode(CueListMode::Cart, Some(false));
    let selected = wait_pad("10");
    let selected_id = selected.id();
    let neighbor = wait_pad("11");
    let neighbor_id = neighbor.id();
    cart.push(selected);
    cart.push(neighbor);
    let list_id = cart.id;
    ws.cue_lists.push(cart);

    let mut second = CueList::new("Crowd");
    second.set_mode(CueListMode::Cart, Some(false));
    let other_pad = wait_pad("10");
    let other_id = other_pad.id();
    second.push(other_pad);
    let second_id = second.id;
    ws.cue_lists.push(second);

    let target = bind_cart(&ws, list_id);
    let other_window = bind_cart(&ws, second_id);
    assert_ne!(target.label(), other_window.label());
    assert!(target.pad_list(&mut ws, other_id, true).is_err());
    assert!(other_window.pad_list(&mut ws, selected_id, true).is_err());
    assert_eq!(
        CueListWindowTarget::from_label(&target.label(), CueListWindowKind::Cart).unwrap(),
        target
    );
    assert!(target.pad_list(&mut ws, standby_id, true).is_err());
    let (context, _rx, _log) = common::recording_context();
    let mut transport = Transport::new(context);
    let cart = target.pad_list(&mut ws, selected_id, true).unwrap();
    assert_eq!(
        transport.go_by_id(cart, &selected_id).unwrap().triggered,
        vec![selected_id]
    );
    assert!(cart.get(&selected_id).unwrap().is_running());
    assert!(!cart.get(&neighbor_id).unwrap().is_running());
    assert!(cart.playhead_cue_id.is_none());
    transport
        .stop_cue(
            target.pad_list(&mut ws, selected_id, false).unwrap(),
            &selected_id,
        )
        .unwrap();
    assert_eq!(ws.active_cue_list_id, main_list);
    assert_eq!(
        ws.active_cue_list().unwrap().playhead_cue_id,
        Some(standby_id)
    );
    assert!(!ws
        .active_cue_list()
        .unwrap()
        .get(&standby_id)
        .unwrap()
        .is_running());
}

#[test]
fn stale_hidden_disarmed_and_legacy_targets_are_rejected() {
    let mut ws = Workspace::new("Show");
    let cart = ws.active_cue_list_mut().unwrap();
    cart.set_mode(CueListMode::Cart, Some(false));
    let cue = wait_pad("1");
    let cue_id = cue.id();
    cart.push(cue);
    cart.cart_layout = cart
        .edited_cart_layout(CartEdit::Resize {
            rows: 2,
            columns: 2,
        })
        .unwrap();
    cart.cart_layout = cart
        .edited_cart_layout(CartEdit::Place {
            cue_id,
            cell: CartCell { row: 2, column: 1 },
        })
        .unwrap();
    let list_id = cart.id;
    let target = bind_cart(&ws, list_id);

    let cart = ws.active_cue_list_mut().unwrap();
    cart.cart_layout = cart
        .edited_cart_layout(CartEdit::Resize {
            rows: 1,
            columns: 2,
        })
        .unwrap();
    assert!(target.pad_list(&mut ws, cue_id, true).is_err());
    let cart = ws.active_cue_list_mut().unwrap();
    cart.cart_layout = cart.edited_cart_layout(CartEdit::Compact).unwrap();
    cart.get_mut(&cue_id).unwrap().set_disabled(true);
    assert!(target.pad_list(&mut ws, cue_id, true).is_err());
    ws.active_cue_list_mut()
        .unwrap()
        .set_mode(CueListMode::Cart, Some(true));
    assert!(target.list(&ws).is_err());
    assert!(CueListWindowTarget::bind(&ws, list_id, CueListWindowKind::Cart).is_err());

    let mut replacement = Workspace::new("Same persisted IDs, fresh document instance");
    let replacement_list = replacement.active_cue_list_mut().unwrap();
    replacement_list.id = list_id;
    replacement_list.set_mode(CueListMode::Cart, Some(false));
    assert!(target.list(&replacement).is_err());
    ws.cue_lists.clear();
    assert!(target.list(&ws).is_err());
    assert!(CueListWindowTarget::from_label("main", CueListWindowKind::Cart).is_err());
}
