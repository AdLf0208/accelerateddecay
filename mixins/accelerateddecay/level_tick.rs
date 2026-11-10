// Runs at the head of `Game::native_level_tick_post` (API 4), with the level
// of `dimension` swapped into place: the only place a `native_set_block` of
// that dimension is accepted.
fn observe_level_tick(game: &mut crate::game::Game<'_>, dimension: &str) {
    crate::game::accelerateddecay_state::on_level_tick(game, dimension);
}
