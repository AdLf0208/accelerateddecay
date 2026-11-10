# accelerateddecay (Horizon port)

A Horizon port of [accelerateddecay](ErrorMikey/AcceleratedDecay) — GPL-3.0-only.

**Status: first slices.** What is below is what the port actually does today.

## Implemented so far

- The mod removes members of `#minecraft:leaves` that are "ready to be decayed"
- (`distance == 7`, `!persistent`) one **real second** after a `#minecraft:logs`
- break. The scan is a single flood fill over the full **26-neighbour** cube
- that expands only through already-ready leaves; it has no radius, budget,
- retry or cap. Each collected leaf is broken outright: loot with the breaking
- player, one block-break level event per tree, `BLOCK_DESTROY`, and the
- fluid's legacy block** — water for a `waterlogged` leaf, air for a dry one.
- The port matches the predicate, the one-second (20-tick) delay, the

## Not yet ported

- Removal is not a full break.** Upstream fires a cancelable
- `BlockEvent.BREAK` per leaf (`:61-64`), drops the block's loot with the
- breaking player (`Block.dropResources`, `:124-125`), plays the block-break
- level event `2001` once per tree (`:119-121`), sets the fluid's legacy block
- with flags `3` (`:128`) and fires `GameEvent.BLOCK_DESTROY` (`:129`). At pin
- `c9d474d` Horizon has **no drop-resources API and no item-entity spawn API**
- (`native_item_entities` is read-only), **no callable cancelable break event**
- (the `destroy_block` hook is a listener, not an event bus), **no level-event

## Install

Build the bundle with a native-mod capable launcher, or add the file from the
registry once it is listed. Pinned to Horizon `1d63e0a` and horizon-mixins
`5a64af93` for game 26.3.

## Build from source

```sh
cargo test
python3 tools/horizon.py check --horizon <horizon checkout>
python3 tools/horizon.py build --horizon <horizon checkout>
python3 tools/package.py
```

## Credits and licence

Original mod: ErrorMikey/AcceleratedDecay. Port: AdLf0208. Licence: GPL-3.0-only — see [LICENSE](LICENSE);
the original notices are preserved.
