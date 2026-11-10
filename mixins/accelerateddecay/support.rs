// Game-side glue for Accelerated Decay: the shared static, the [`Reader`]
// implementation over Horizon's server APIs, and the three entry functions
// the hook helpers call. Injected into `crate::game` as
// `accelerateddecay_state`; it needs the game, so it is not part of the
// standalone library (only `runtime.rs` is).
//
// Everything here runs on the server thread inside the named hooks:
// `on_destroy` in `native_destroy_block`, `on_server_tick` in
// `native_server_tick` (read scope granted), `on_level_tick` in
// `native_level_tick_post` (that level in place, writes accepted).

use std::sync::Mutex;

use horizon_world::chunk::{BlockProperties, BlockState};
use horizon_world::level::flags;
use horizon_world::registry::BlockStateId;

use crate::game::accelerateddecay::{Engine, Reader, Removal, Snapshot};
use crate::game::native_lifecycle::{NativeLifecycleContext, NativeWorldEpoch};

/// Cross-hook state: one engine, the server-tick counter it schedules on,
/// the world epoch it belongs to, and the removals waiting for their level
/// pass. `None` until the first hook fires.
static CORE: Mutex<Option<Core>> = Mutex::new(None);

struct Core {
    /// The open attempt the state belongs to. A different epoch (a new world)
    /// drops everything: the anchors of a closed world must never touch a
    /// later one's blocks.
    epoch: Option<NativeWorldEpoch>,
    /// Our own running tick count, incremented once per running server tick.
    /// (`NativeTickId` has no public accessor at the pinned revision.)
    tick: i64,
    engine: Engine,
    /// Removals decided by the engine, executed by the next level pass of
    /// their dimension.
    pending: Vec<Removal>,
}

impl Default for Core {
    fn default() -> Self {
        Core {
            epoch: None,
            tick: 0,
            engine: Engine::new(),
            pending: Vec::new(),
        }
    }
}

fn core() -> std::sync::MutexGuard<'static, Option<Core>> {
    CORE.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// A log was destroyed at `pos`: open a felled-tree merge when the block is
/// a member of the merged `#minecraft:logs` tag. Server-side; the acting
/// player's dimension decides which level the merge belongs to.
#[allow(private_interfaces)]
pub fn on_destroy(game: &mut crate::game::Game<'_>, pos: [i32; 3], state: BlockStateId) {
    let Some(player) = game.native_acting_player() else {
        return;
    };
    let Some(block) = game.pack.assets.registry.state(state) else {
        return;
    };
    // Merged server tags, no read scope required: this is the `ServerLevel`
    // tag set. Unknown or unavailable tags mean "not a log": do nothing,
    // which is also what the original does for unsupported trees.
    let Ok(is_log) = game.native_server_block_in_tag(block, "minecraft:logs") else {
        return;
    };
    if !is_log {
        return;
    }
    let dimension = player.dimension;
    let mut guard = core();
    let core = guard.get_or_insert_with(Core::default);
    let tick = core.tick;
    core.engine.on_log_removed(&dimension, pos, tick);
}

/// One server tick. Only the start of a running tick does work: read the
/// neighbourhoods the engine asks for and queue the leaves it declares
/// ready; the level passes that follow execute the removals while their
/// level is in place.
#[allow(private_interfaces)]
pub fn on_server_tick(
    game: &mut crate::game::Game<'_>,
    context: &NativeLifecycleContext,
    phase: crate::game::native_lifecycle::ServerTickPhase,
) {
    let crate::game::native_lifecycle::ServerTickPhase::ServerTickStart { world_running } = phase
    else {
        return;
    };
    if !world_running {
        return;
    }
    let mut guard = core();
    let core = guard.get_or_insert_with(Core::default);
    if core.epoch != Some(context.epoch) {
        core.epoch = Some(context.epoch);
        core.tick = 0;
        core.engine = Engine::new();
        core.pending.clear();
    }
    core.tick += 1;
    let tick = core.tick;
    let mut reader = ServerReader { game, context };
    let removals = core.engine.step(tick, &mut reader);
    core.pending.extend(removals);
}

/// The level `dimension` just ticked, with that level in place: replace the
/// queued leaves of this dimension with air, or with water when the leaf was
/// waterlogged (upstream sets the fluid's legacy block, `:128`).
/// `UPDATE_ALL` keeps the game's neighbour and shape updates running, as a
/// normal removal does.
#[allow(private_interfaces)]
pub fn on_level_tick(game: &mut crate::game::Game<'_>, dimension: &str) {
    let mut guard = core();
    let Some(core) = guard.as_mut() else {
        return;
    };
    if core.pending.is_empty() {
        return;
    }
    let Some(air) = game.pack.assets.registry.default_state("minecraft:air") else {
        return;
    };
    let water = game
        .pack
        .assets
        .registry
        .default_state("minecraft:water");
    let mut i = 0;
    while i < core.pending.len() {
        if core.pending[i].dimension == dimension {
            let removal = core.pending.swap_remove(i);
            // A registry without `minecraft:water` is malformed; fall back to
            // air rather than skip the removal (the leaf must still go).
            let target = if removal.waterlogged {
                water.unwrap_or(air)
            } else {
                air
            };
            game.native_set_block(dimension, removal.pos, target, flags::UPDATE_ALL);
        } else {
            i += 1;
        }
    }
}

/// The engine's view of the server: reads go through `native_server_block`
/// by dimension (the hook's read scope permits them, including parked
/// levels), tag membership through the merged server tags.
struct ServerReader<'a, 'g, 'c> {
    game: &'a mut crate::game::Game<'g>,
    context: &'c NativeLifecycleContext,
}

impl Reader for ServerReader<'_, '_, '_> {
    fn read(&mut self, dimension: &str, pos: [i32; 3]) -> Option<Snapshot> {
        let state = self
            .game
            .native_server_block(self.context, dimension, pos)
            .ok()?;
        let distance = state
            .properties
            .get("distance")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let persistent = state
            .properties
            .get("persistent")
            .is_some_and(|value| value == "true");
        let waterlogged = state
            .properties
            .get("waterlogged")
            .is_some_and(|value| value == "true");
        Some(Snapshot {
            name: state.name,
            distance,
            persistent,
            waterlogged,
        })
    }

    fn is_leaves(&mut self, name: &str) -> bool {
        // `native_block_in_tag` reads only the name from the state.
        let state = BlockState {
            name: name.to_owned(),
            properties: BlockProperties::new(),
        };
        self.game
            .native_block_in_tag(self.context, &state, "minecraft:leaves")
            .unwrap_or(false)
    }
}
