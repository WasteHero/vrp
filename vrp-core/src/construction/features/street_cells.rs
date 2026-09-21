//! COMPASS-1429: process-global map from a core Location (usize) to a coarse
//! grid-cell id, set once per solve by the pragmatic layer (which holds the
//! coordinates the core does not). The transport objective reads it to fold a
//! "leaving a cell/street" penalty into the minimised cost. Std-only (no deps).
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

fn cells() -> &'static RwLock<HashMap<usize, u64>> {
    static C: OnceLock<RwLock<HashMap<usize, u64>>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(HashMap::new()))
}

fn penalty() -> &'static RwLock<f64> {
    static P: OnceLock<RwLock<f64>> = OnceLock::new();
    P.get_or_init(|| RwLock::new(0.0))
}

/// Replace the cell map and penalty for the upcoming solve.
pub fn set_street_cells(map: HashMap<usize, u64>, factor: f64) {
    *cells().write().unwrap() = map;
    *penalty().write().unwrap() = factor;
}

/// Penalty factor (0 = feature off).
pub fn street_penalty() -> f64 {
    *penalty().read().unwrap()
}

/// Cell id for a location, or u64::MAX when unknown.
pub fn cell_of(loc: usize) -> u64 {
    *cells().read().unwrap().get(&loc).unwrap_or(&u64::MAX)
}
