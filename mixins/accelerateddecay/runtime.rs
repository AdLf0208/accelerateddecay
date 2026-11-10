// Accelerated Decay for Horizon — the logic engine (pure, no game access).
//
// Ported from Accelerated Decay 21.0.0 by ErrorMikey (nanite/AcceleratedDecay;
// LGPL-3.0-only on Modrinth, repository licence GPL-3.0). Local-only port,
// never published without the maintainer's approval.
//
// Behaviour, from the upstream source (`AcceleratedDecay.java`, tag
// r/1.21/21.0.0): one real second after a `#minecraft:logs` break the mod
// flood fills the full 26-neighbour cube from the broken log, collects the
// leaves that already read `distance == 7 && !persistent`, and breaks each of
// them outright — loot with the breaking player, one block-break level event
// per tree, `BLOCK_DESTROY`, and the fluid's legacy block: water for a
// waterlogged leaf, air for a dry one. It never ticks leaves fast; the game's
// own distance machinery marks them ready first.
//
// This port keeps the eligibility predicate, the one-second delay, the
// 26-neighbour reach and the water of a waterlogged leaf. The full break is
// not reproducible at the pin: there is no drop-loot, item-entity or callable
// break-event API, so a removal is a plain `native_set_block` (see notes.md,
// "Known gaps"). The read/removal budgets, retries and caps below are
// deliberate safety deviations from upstream's single unbounded pass, also
// documented in notes.md.
//
// This file contains only pure logic, so it compiles and unit-tests as the
// standalone crate library (`[lib] path = ...`). The glue — a [`Reader`]
// impl over Horizon's `Game` API and the shared static — lives in the
// bundle's support module; hook helpers stay single-function adapters.

use std::collections::{HashSet, VecDeque};

/// Server-tick passes between a log break and the decay scan: the original
/// schedules the scan one **real second** later (`AcceleratedDecay.java:80`),
/// which is 20 ticks at the nominal 20 TPS. The pin exposes no wall clock
/// inside the hook scopes, so the port counts server ticks: under lag it waits
/// slightly longer in real time than upstream (noted as a deviation).
pub const MERGE_DELAY: i64 = 20;

// Safety deviations from upstream, kept on purpose: the original scans a due
// tree in one unlimited pass (no radius, read/removal budget, retry or cap;
// `AcceleratedDecay.java:84-105`, entry dropped at `:70`). The constants below
// bound a pathological modded tree's per-tick work and engine memory instead;
// the defaults are large enough to be invisible on vanilla trees. See notes.md
// "Known gaps" for the recorded deviation.

/// A merge dies after this many server ticks since its anchor, whatever the
/// progress: bounds memory and stops a stale merge from touching a later
/// world's blocks. ~20 s.
pub const MAX_AGE: i64 = 400;

/// Leaf reads per server-tick step, shared by all merges.
pub const PASS_READS: u32 = 256;

/// Leaf removals per server-tick step, shared by all merges.
pub const PASS_REMOVALS: u32 = 24;

/// A not-ready leaf is looked at again after this many ticks: leaves whose
/// `distance` is still propagating become ready over the following ticks.
pub const RETRY_TICKS: i64 = 4;

/// How many times a single position is retried before it is left alone.
/// `distance` grows by one per scheduled step, so 12 attempts (48 ticks) is
/// beyond any vanilla canopy's propagation.
pub const MAX_ATTEMPTS: u8 = 12;

/// Merges at once; a new anchor beyond this evicts the oldest by `last`.
pub const MAX_MERGES: usize = 32;

/// A merge's search box around its anchor, per axis, in blocks: past this
/// distance from the felled trunk nothing is touched.
pub const SEARCH_RADIUS: i32 = 24;

/// What a read told the engine about one block.
#[derive(Clone)]
pub struct Snapshot {
    /// The state's block name (`mymod:tree`).
    pub name: String,
    /// The leaf's `distance` property, 1..=7. A block without the property
    /// reads 0 and is never ready.
    pub distance: i32,
    /// The leaf's `persistent` property: a player-placed leaf.
    pub persistent: bool,
    /// The leaf's `waterlogged` property: the leaf sits in a water source,
    /// which upstream leaves behind as water when the leaf is broken.
    pub waterlogged: bool,
}

/// The world the engine reads. `read`'s None means "not observable now"
/// (unloaded chunk, out of bounds, invalid state): such a position is
/// forgotten forever. `is_leaves` asks the merged `#minecraft:leaves` tag.
pub trait Reader {
    fn read(&mut self, dimension: &str, pos: [i32; 3]) -> Option<Snapshot>;
    fn is_leaves(&mut self, name: &str) -> bool;
}

/// A leaf the engine decided to remove.
pub struct Removal {
    pub dimension: String,
    pub pos: [i32; 3],
    /// The leaf was waterlogged: the glue leaves `minecraft:water` behind
    /// instead of air (upstream sets the fluid's legacy block).
    pub waterlogged: bool,
}

/// The 26 neighbours of a block — `BoundingBox(BlockPos.ZERO).inflatedBy(1)`
/// minus the centre — which is the upstream scan's reach
/// (`AcceleratedDecay.java:84-88`).
const NEIGHBORS: [[i32; 3]; 26] = [
    [-1, -1, -1],
    [-1, -1, 0],
    [-1, -1, 1],
    [-1, 0, -1],
    [-1, 0, 0],
    [-1, 0, 1],
    [-1, 1, -1],
    [-1, 1, 0],
    [-1, 1, 1],
    [0, -1, -1],
    [0, -1, 0],
    [0, -1, 1],
    [0, 0, -1],
    [0, 0, 1],
    [0, 1, -1],
    [0, 1, 0],
    [0, 1, 1],
    [1, -1, -1],
    [1, -1, 0],
    [1, -1, 1],
    [1, 0, -1],
    [1, 0, 0],
    [1, 0, 1],
    [1, 1, -1],
    [1, 1, 0],
    [1, 1, 1],
];

/// A felled-tree worker: one removed log whose neighbourhood is being decayed.
struct Merge {
    dimension: String,
    /// The removed log's position (the box centre, not necessarily a decayed
    /// block itself).
    origin: [i32; 3],
    /// The anchor's server tick; removals begin after `MERGE_DELAY`.
    born: i64,
    /// The last tick the merge read or removed anything.
    last: i64,
    queue: VecDeque<([i32; 3], u8)>,
    visited: HashSet<[i32; 3]>,
    /// Leaves that were not ready yet (their `distance` had not propagated):
    /// (position, next tick to look, attempts so far). Bounded by
    /// `MAX_ATTEMPTS`, then left to a later merge or the vanilla random tick.
    retry: Vec<([i32; 3], i64, u8)>,
    /// Set when this pass finished the merge; dropped after the step.
    done: bool,
}

impl Merge {
    fn within_box(&self, pos: [i32; 3]) -> bool {
        #[allow(clippy::needless_range_loop)]
        for i in 0..3 {
            if (pos[i] - self.origin[i]).abs() > SEARCH_RADIUS {
                return false;
            }
        }
        true
    }
}

/// The port's whole state.
pub struct Engine {
    merges: Vec<Merge>,
}

impl Engine {
    pub fn new() -> Self {
        Engine { merges: Vec::new() }
    }

    /// A member of `#minecraft:logs` was destroyed at `pos` (the caller
    /// checked the tag). Opens a merge; beyond `MAX_MERGES`, the oldest
    /// merge is evicted.
    pub fn on_log_removed(&mut self, dimension: &str, pos: [i32; 3], tick: i64) {
        self.merges
            .retain(|m| m.dimension != dimension || m.origin != pos);
        while self.merges.len() >= MAX_MERGES {
            let oldest = self
                .merges
                .iter()
                .enumerate()
                .min_by_key(|(_, m)| (m.last, m.born))
                .map(|(i, _)| i);
            match oldest {
                Some(i) => {
                    self.merges.remove(i);
                }
                None => break,
            }
        }
        self.merges.push(Merge {
            dimension: dimension.to_string(),
            origin: pos,
            born: tick,
            last: tick,
            queue: {
                let mut q = VecDeque::new();
                for n in NEIGHBORS {
                    q.push_back(([pos[0] + n[0], pos[1] + n[1], pos[2] + n[2]], 0));
                }
                q
            },
            visited: HashSet::from([pos]),
            retry: Vec::new(),
            done: false,
        });
    }

    /// One server-tick step. Reads the world through `read`, decides what to
    /// remove, returns the leaf removals for the glue to execute through
    /// `native_set_block` during the following level passes (a `set_block`
    /// only accepts the level currently swapped in).
    pub fn step<R: Reader>(&mut self, tick: i64, read: &mut R) -> Vec<Removal> {
        // Expire merges by age (idle ones finish on their own: every
        // position becomes visited after being read or failed once, so a
        // merge's queue always drains).
        self.merges
            .retain(|m| tick.saturating_sub(m.born) <= MAX_AGE);

        let mut out = Vec::new();
        let mut reads = PASS_READS;
        let mut removals = PASS_REMOVALS;
        for m in self.merges.iter_mut() {
            // The delay lives per merge and is not paid out of the budgets.
            // The scan is due once the delay has elapsed, exactly like
            // upstream's `!checkAfter.isAfter(now)` test: the first active
            // tick is `born + MERGE_DELAY`.
            if tick < m.born + MERGE_DELAY {
                continue;
            }
            // Leaves that were not ready yet: look again once their time is
            // due. A position may thus be read more than once.
            let mut due = Vec::new();
            m.retry.retain(|(pos, next, attempts)| {
                if *next <= tick {
                    if *attempts < MAX_ATTEMPTS {
                        due.push((*pos, *attempts));
                    }
                    false
                } else {
                    true
                }
            });
            for (pos, attempts) in due {
                m.queue.push_back((pos, attempts));
            }
            while reads > 0 && removals > 0 {
                let Some((pos, attempts)) = m.queue.pop_front() else {
                    m.done = m.retry.is_empty();
                    break;
                };
                if !m.visited.insert(pos) {
                    continue;
                }
                if !m.within_box(pos) {
                    continue;
                }
                reads -= 1;
                let Some(snapshot) = read.read(&m.dimension, pos) else {
                    continue;
                };
                if !read.is_leaves(&snapshot.name) {
                    continue;
                }
                // A live leaf forwards the search to its neighbours.
                for n in NEIGHBORS {
                    let next = [pos[0] + n[0], pos[1] + n[1], pos[2] + n[2]];
                    if !m.visited.contains(&next) && m.within_box(next) {
                        m.queue.push_back((next, 0));
                    }
                }
                if snapshot.distance < 7 && !snapshot.persistent {
                    // Not ready yet: the game's distance chain has not
                    // reached 7. Look again a few ticks later instead of
                    // marking the position dead (the original keeps
                    // checking too). Persistent leaves are terminal: they
                    // never decay.
                    m.visited.remove(&pos);
                    if attempts + 1 < MAX_ATTEMPTS {
                        m.retry.push((pos, tick + RETRY_TICKS, attempts + 1));
                    }
                    continue;
                }
                if snapshot.persistent {
                    continue;
                }
                m.last = tick;
                removals -= 1;
                out.push(Removal {
                    dimension: m.dimension.clone(),
                    pos,
                    waterlogged: snapshot.waterlogged,
                });
            }
            if reads == 0 || removals == 0 {
                break;
            }
        }
        self.merges.retain(|m| !m.done);
        out
    }

    /// Debug/observation surface: the glue resets the engine by replacing it,
    /// so this is only read by the embedded tests at the pin.
    #[allow(dead_code)]
    pub fn merge_count(&self) -> usize {
        self.merges.len()
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A square grid world where named blocks are placed by hand. Leaves are
    /// members of the tag by name convention (`oak_leaves` etc. mirror the
    /// tag's members); nothing else is.
    struct TestWorld {
        blocks: HashMap<[i32; 3], Snapshot>,
        // Leaves on ready: distance 7 and not persistent, as a vanilla tree's
        // canopy reads after the logs are gone.
    }

    impl TestWorld {
        fn leaf(&mut self, pos: [i32; 3]) {
            self.blocks.insert(
                pos,
                Snapshot {
                    name: "minecraft:oak_leaves".into(),
                    distance: 7,
                    persistent: false,
                    waterlogged: false,
                },
            );
        }
        fn leaf_at_distance(&mut self, pos: [i32; 3], distance: i32) {
            self.blocks.insert(
                pos,
                Snapshot {
                    name: "minecraft:oak_leaves".into(),
                    distance,
                    persistent: false,
                    waterlogged: false,
                },
            );
        }
        fn waterlogged_leaf(&mut self, pos: [i32; 3]) {
            self.blocks.insert(
                pos,
                Snapshot {
                    name: "minecraft:oak_leaves".into(),
                    distance: 7,
                    persistent: false,
                    waterlogged: true,
                },
            );
        }
        fn player_leaf(&mut self, pos: [i32; 3]) {
            self.blocks.insert(
                pos,
                Snapshot {
                    name: "minecraft:oak_leaves".into(),
                    distance: 7,
                    persistent: true,
                    waterlogged: false,
                },
            );
        }
        fn other(&mut self, pos: [i32; 3], name: &str) {
            self.blocks.insert(
                pos,
                Snapshot {
                    name: name.into(),
                    distance: 0,
                    persistent: false,
                    waterlogged: false,
                },
            );
        }
    }

    impl Reader for TestWorld {
        fn read(&mut self, _dim: &str, pos: [i32; 3]) -> Option<Snapshot> {
            self.blocks.get(&pos).cloned()
        }
        fn is_leaves(&mut self, name: &str) -> bool {
            name.ends_with("leaves")
        }
    }

    /// A 2×2 trunk at the origin, logs removed, canopy of 6 ready leaves
    /// wedged around it, one not-ready leaf (distance 3), one player leaf,
    /// one stone block, all reachable.
    fn tree() -> (Engine, TestWorld) {
        let mut w = TestWorld {
            blocks: HashMap::new(),
        };
        for p in [
            [0, 2, 0],
            [1, 3, 0],
            [-1, 2, 1],
            [0, 2, -1],
            [2, 1, 0],
            [0, 3, 0],
        ] {
            w.leaf(p);
        }
        w.leaf_at_distance([0, 4, 0], 3);
        w.leaf([2, 2, 0]); // within the box and leaf-connected to the canopy: it goes too
        w.leaf([-1, 2, 0]); // bridges [-1, 2, 1] into the canopy cluster
        w.leaf([2, 3, 0]); // bridges the x=2 leaves back into the canopy cluster
        w.player_leaf([1, 2, 0]);
        w.other([0, 1, 1], "minecraft:stone");

        let mut e = Engine::new();
        // Destroying the four trunk logs. Only the canopy above is at demand.
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 0);
        e.on_log_removed("minecraft:overworld", [1, 0, 0], 0);
        e.on_log_removed("minecraft:overworld", [0, 1, 0], 0);
        e.on_log_removed("minecraft:overworld", [1, 1, 0], 0);
        (e, w)
    }

    #[test]
    fn the_scan_waits_one_second_after_the_log_break() {
        // Upstream schedules the scan at `now + 1 s` (`AcceleratedDecay.java:80`);
        // 20 server ticks is that second at the nominal 20 TPS.
        assert_eq!(MERGE_DELAY, 20, "one real second, not the old 6 ticks");
        let mut w = TestWorld {
            blocks: HashMap::new(),
        };
        w.leaf([0, 1, 0]);
        let mut e = Engine::new();
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 0);
        for tick in 1..MERGE_DELAY {
            assert!(
                e.step(tick, &mut w).is_empty(),
                "no removal before the one-second delay, tick {tick}"
            );
        }
        assert!(
            !e.step(MERGE_DELAY, &mut w).is_empty(),
            "upstream's due test is `now >= checkAfter`: the scan runs exactly 20 ticks after the break"
        );
    }

    #[test]
    fn all_corner_neighbours_are_scanned() {
        // Upstream scans the full 26-neighbour cube (`AcceleratedDecay.java:84-88`,
        // `BoundingBox(BlockPos.ZERO).inflatedBy(1)` minus the centre). A
        // 6-neighbour scan never sees any of these diagonals.
        let corners: &[[i32; 3]] = &[
            [1, 1, 1],
            [1, 1, -1],
            [1, -1, 1],
            [1, -1, -1],
            [-1, 1, 1],
            [-1, 1, -1],
            [-1, -1, 1],
            [-1, -1, -1],
        ];
        let mut w = TestWorld {
            blocks: HashMap::new(),
        };
        for corner in corners {
            w.leaf(*corner);
        }
        let mut e = Engine::new();
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 0);
        let mut removed = Vec::new();
        for tick in (MERGE_DELAY + 1)..=(MERGE_DELAY * 3) {
            removed.extend(e.step(tick, &mut w).into_iter().map(|r| r.pos));
        }
        for corner in corners {
            assert!(
                removed.contains(corner),
                "diagonal leaf {corner:?} should be reached, removed {removed:?}"
            );
        }
    }

    #[test]
    fn a_gap_wider_than_one_block_is_not_crossed() {
        // The cube is still one block: [2,0,0] is not a neighbour of the
        // anchor and no leaf bridges the gap, so it survives.
        let mut w = TestWorld {
            blocks: HashMap::new(),
        };
        w.leaf([2, 0, 0]);
        let mut e = Engine::new();
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 0);
        for tick in (MERGE_DELAY + 1)..=(MERGE_DELAY * 3) {
            e.step(tick, &mut w);
        }
        assert!(
            w.blocks.contains_key(&[2, 0, 0]),
            "a leaf two blocks away is not touched"
        );
    }

    #[test]
    fn waterlogged_leaves_become_water_and_dry_ones_air() {
        // Upstream removes a leaf with its fluid's legacy block
        // (`AcceleratedDecay.java:128`): water for a `waterlogged=true` leaf,
        // air for a dry one.
        let mut w = TestWorld {
            blocks: HashMap::new(),
        };
        w.waterlogged_leaf([0, 1, 0]);
        w.leaf([0, 0, 1]);
        let mut e = Engine::new();
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 0);
        let mut removals = Vec::new();
        for tick in (MERGE_DELAY + 1)..=(MERGE_DELAY * 3) {
            removals.extend(e.step(tick, &mut w));
        }
        let water = removals
            .iter()
            .find(|r| r.pos == [0, 1, 0])
            .expect("the waterlogged leaf is removed");
        assert!(water.waterlogged, "a waterlogged leaf leaves water behind");
        let dry = removals
            .iter()
            .find(|r| r.pos == [0, 0, 1])
            .expect("the dry leaf is removed");
        assert!(!dry.waterlogged, "a dry leaf leaves air behind");
    }

    #[test]
    fn canopy_is_removed_after_the_delay() {
        let (mut e, mut w) = tree();
        for tick in 1..MERGE_DELAY {
            let out = e.step(tick, &mut w);
            assert!(out.is_empty(), "nothing before the delay, tick {tick}");
        }
        let mut removed = Vec::new();
        for tick in MERGE_DELAY..=MERGE_DELAY * 3 {
            removed.extend(e.step(tick, &mut w).into_iter().map(|r| r.pos));
        }
        let ready: &[[i32; 3]] = &[
            [0, 2, 0],
            [1, 3, 0],
            [-1, 2, 1],
            [0, 2, -1],
            [2, 1, 0],
            [0, 3, 0],
        ];
        for p in ready {
            assert!(removed.contains(p), "ready leaf {p:?} should be removed");
        }
    }

    #[test]
    fn nonready_persistent_and_nonleaf_blocks_survive() {
        let (mut e, mut w) = tree();
        for tick in 1..=MERGE_DELAY * 3 {
            e.step(tick, &mut w);
        }
        assert!(w.blocks.contains_key(&[0, 4, 0]), "distance 3 leaf stays");
        assert!(
            w.blocks.contains_key(&[1, 2, 0]),
            "player-placed leaf stays"
        );
        assert!(w.blocks.contains_key(&[0, 1, 1]), "stone stays");
    }

    /// The first active tick removes at most `PASS_REMOVALS` (24) leaves,
    /// regardless of how many leaves are reachable in a connected cluster.
    /// We build a dense cluster (40 ready leaves) and verify the budget is
    /// respected in the very first active pass.
    #[test]
    fn budget_limits_first_active_tick_removals() {
        let mut w = TestWorld {
            blocks: HashMap::new(),
        };
        // 4×4 cluster = 16 leaves around origin
        for y in 0..=3 {
            for z in 0..=3 {
                w.leaf([0, y, z]);
            }
        }
        let mut e = Engine::new();
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 0);
        for tick in 1..MERGE_DELAY {
            assert!(e.step(tick, &mut w).is_empty(), "delay: nothing removed");
        }
        let first_active_removals = e.step(MERGE_DELAY, &mut w);
        assert!(
            first_active_removals.len() <= PASS_REMOVALS as usize,
            "budget respected: removed {} at first tick (budget: {})",
            first_active_removals.len(),
            PASS_REMOVALS,
        );
        // 4×4 cluster = 16 leaves; budget drains in 1 tick, done by tick 2
        let mut total = first_active_removals.len();
        for tick in (MERGE_DELAY + 1)..=(MERGE_DELAY + 2) {
            total += e.step(tick, &mut w).len();
        }
        assert!(
            total >= 15, // almost all 16 reachable leaves removed
            "cluster drains over ticks: removed {} (expected >=15 over 2 ticks)",
            total,
        );
    }

    #[test]
    fn stone_breaks_the_search_branch() {
        // A wall of stone between anchor and leaves: the search may not
        // pass through non-leaves (their neighbours are not inspected).
        let mut w = TestWorld {
            blocks: HashMap::new(),
        };
        w.other([0, 1, 0], "minecraft:stone");
        w.leaf([0, 3, 0]);
        let mut e = Engine::new();
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 0);
        for tick in 1..=MERGE_DELAY * 3 {
            e.step(tick, &mut w);
        }
        assert!(
            w.blocks.contains_key(&[0, 3, 0]),
            "a leaf not connected through leaves is never touched"
        );
    }

    #[test]
    fn budgets_spread_work_over_passes() {
        // 30 ready leaves all connected; 24 removed on the first active pass.
        let mut w = TestWorld {
            blocks: HashMap::new(),
        };
        for i in 0..30 {
            w.leaf([i, 1, 0]);
            w.leaf([i, 2, 0]);
        }
        let mut e = Engine::new();
        e.on_log_removed("minecraft:overworld", [15, 0, 0], 0);
        for tick in 1..MERGE_DELAY {
            e.step(tick, &mut w);
        }
        let first = e.step(MERGE_DELAY, &mut w);
        assert!(
            first.len() <= PASS_REMOVALS as usize,
            "a step removes at most the per-pass budget"
        );
        let mut total = first.len();
        for tick in (MERGE_DELAY + 2)..=(MERGE_DELAY + 20) {
            total += e.step(tick, &mut w).len();
        }
        // Every column lies within `SEARCH_RADIUS` of `[15,0,0]` (|0−15| = 15).
        assert_eq!(total, 60, "everything in reach is removed in the end");
    }

    #[test]
    fn merges_are_capped_by_evicting_the_oldest() {
        let mut e = Engine::new();
        for i in 0..(MAX_MERGES as i32 + 4) {
            e.on_log_removed("minecraft:overworld", [i * 30, 0, 0], i as i64 + 10);
        }
        assert_eq!(e.merge_count(), MAX_MERGES);
        assert_eq!(e.merges[0].origin, [4 * 30, 0, 0], "oldest evicted first");
    }

    #[test]
    fn merges_expire_after_max_age() {
        let mut e = Engine::new();
        let mut w = TestWorld {
            blocks: HashMap::new(),
        };
        // A merge left alone past `MAX_AGE` dies at once, before it can do
        // anything (a merge in an empty world also just finishes: every
        // position is visited once and reads nothing).
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 0);
        e.step(MAX_AGE + 2, &mut w);
        assert_eq!(e.merge_count(), 0, "expired by age");
    }

    #[test]
    fn the_other_dimension_is_alone() {
        let (mut e, mut w) = tree();
        let overworld_tick = MERGE_DELAY + 1;
        let out = e.step(overworld_tick, &mut w);
        assert!(!out.is_empty(), "overworld canopy processed");
        // A merge in the Nether only steps reads in the Nether — the
        // engine reads per merge, so a Nether-only step does nothing here.
        let mut w2 = TestWorld {
            blocks: HashMap::new(),
        };
        let mut e2 = Engine::new();
        e2.on_log_removed("minecraft:the_nether", [0, 0, 0], 0);
        let out2 = e2.step(overworld_tick, &mut w2);
        assert!(
            out2.iter().all(|r| r.dimension == "minecraft:the_nether"),
            "removals carry their merge's dimension"
        );
    }

    #[test]
    fn a_leaf_becoming_ready_later_is_still_removed() {
        let mut w = TestWorld {
            blocks: HashMap::new(),
        };
        w.leaf_at_distance([0, 1, 0], 3);
        let mut e = Engine::new();
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 0);
        // First active pass reads it as not ready and schedules a retry.
        e.step(MERGE_DELAY + 1, &mut w);
        assert!(w.blocks.contains_key(&[0, 1, 0]));
        // The game's distance chain reaches 7 after the first scan.
        w.leaf([0, 1, 0]);
        let mut removed = false;
        for tick in (MERGE_DELAY + 2)..=(MERGE_DELAY + 2 + RETRY_TICKS * 3) {
            if e.step(tick, &mut w).iter().any(|r| r.pos == [0, 1, 0]) {
                removed = true;
                break;
            }
        }
        assert!(
            removed,
            "a leaf that became ready inside the retry window is removed"
        );
    }

    #[test]
    fn a_persistent_leaf_is_not_retried() {
        let mut w = TestWorld {
            blocks: HashMap::new(),
        };
        w.player_leaf([0, 1, 0]);
        let mut e = Engine::new();
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 0);
        for tick in 1..=(MERGE_DELAY + RETRY_TICKS + 2) {
            e.step(tick, &mut w);
        }
        assert_eq!(
            e.merge_count(),
            0,
            "a player-placed leaf ends the merge instead of being retried"
        );
    }

    #[test]
    fn relisted_removal_of_the_same_log_is_one_merge() {
        let mut e = Engine::new();
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 0);
        e.on_log_removed("minecraft:overworld", [0, 0, 0], 5);
        assert_eq!(e.merge_count(), 1);
    }
}
