// Runs at the head of `Game::native_server_tick` (API 4). The game grants the
// hook's read scope around this call, so the glue may read blocks by
// dimension; the engine only steps on `ServerTickStart` with a running
// world, and only queues removals (the level passes execute them).
fn observe_server_tick(
    game: &mut crate::game::Game<'_>,
    context: &NativeLifecycleContext,
    phase: ServerTickPhase,
) {
    crate::game::accelerateddecay_state::on_server_tick(game, context, phase);
}
