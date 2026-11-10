// Runs at the head of `Game::native_destroy_block` (API 14): the player's
// break was accepted, the block is still in place. Observation only — the
// helper never sets `canceled`, so the break proceeds unchanged.
fn observe_destroy(
    game: &mut crate::game::Game<'_>,
    pos: [i32; 3],
    state: BlockStateId,
) {
    crate::game::accelerateddecay_state::on_destroy(game, pos, state);
}
