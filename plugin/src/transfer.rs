use crate::game::{Action, Power};
use std::{collections::BTreeMap, sync::{Mutex, MutexGuard}};

type Transfers = BTreeMap<String, Transfer>;

#[derive(Default)]
struct Transfer {
	ready: bool,
	queued: Option<Power>,
}

static TRANSFERS: Mutex<BTreeMap<String, Transfer>> = Mutex::new(BTreeMap::new());

// Java's movement handler ignores position packets until the current teleport
// has been confirmed. An accepted PlayerMoveEvent therefore lets us send the
// next plugin teleport without overwriting Pumpkin's awaiting_teleport slot.
pub fn moved(id: &str) {
	TRANSFERS.lock().unwrap_or_else(|error| error.into_inner()).entry(id.to_string()).or_default().ready = true;
}

pub fn requested(id: &str) {
	TRANSFERS.lock().unwrap_or_else(|error| error.into_inner()).entry(id.to_string()).or_default().ready = false;
}

pub fn defer(id: &str, power: Power) -> Option<Power> {
	let mut transfers: MutexGuard<'_, Transfers> = TRANSFERS.lock().unwrap_or_else(|error| error.into_inner());
	let state: &mut Transfer = transfers.entry(id.to_string()).or_default();
	if state.ready {
		state.ready = false;
		state.queued = None;
		Some(power)
	} else {
		// Keep the latest destination (e.g. stopping a round supersedes a warp).
		state.queued = Some(power);
		None
	}
}

pub fn drain() -> Vec<Action> {
	let mut transfers: MutexGuard<'_, Transfers> = TRANSFERS.lock().unwrap_or_else(|error| error.into_inner());
	transfers.iter_mut().filter_map(|(id, state): (&String, &mut Transfer)| {
		if state.ready { state.queued.take().map(|power: Power| Action::Player(id.clone(), power)) } else { None }
	}).collect()
}

pub fn forget(id: &str) {
	TRANSFERS.lock().unwrap_or_else(|error| error.into_inner()).remove(id);
}

pub fn cancel(id: &str) {
	if let Some(state) = TRANSFERS.lock().unwrap_or_else(|error| error.into_inner()).get_mut(id) { state.queued = None; }
}

pub fn clear() {
	TRANSFERS.lock().unwrap_or_else(|error| error.into_inner()).clear();
}
