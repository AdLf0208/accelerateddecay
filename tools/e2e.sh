#!/usr/bin/env bash
# In-game check for the Accelerated Decay port.
#
# Random ticks are turned OFF (gamerule random_tick_speed 0), so vanilla leaf
# decay cannot fire: any leaf the port removes is provably its work. We fell a
# 3-log column, plant ready-to-decay leaves (distance 7, not persistent) in its
# old neighbours, and also one control leaf 10 blocks away that belongs to no
# felled tree. A second, separate column plants a *waterlogged* ready leaf:
# upstream replaces a leaf with its fluid's legacy block, so that position must
# read minecraft:water afterwards, not air. The run ends with a verdict: no leaf
# or log may be left in the box around the column, the control leaf must still
# be oak_leaves, and the waterlogged leaf's position must be water.
#
#   tools/e2e.sh            # uses the fast build under target/horizon-cargo
#   SELFTEST=1 tools/e2e.sh # repeats the scenario, then asserts that a removed
#                           # column leaf is still present: the run MUST fail
#                           # (exit 1), proving the verdict can fail.
#
# Needs the prepared 26.3 resources (RES) and a built horizon-client +
# horizon-ctl in the port's fast target (or CTL/GAME overrides).
set -euo pipefail

CLONE=${CAMPAIGN_ROOT:-$(cd "$(dirname "$0")/../../.." && pwd)}
PORT=$CLONE/ports/accelerateddecay
CTL=${CTL:-$PORT/target/horizon-cargo/fast/horizon-ctl}
GAME=${GAME:-$PORT/target/horizon-cargo/fast/horizon-client}
RES=${RES:-${HORIZON_RES:-${HORIZON_RES:-$HOME/.local/horizon-client/26.3}}}
SOCK=${SOCK:-/tmp/opencode/accdecay-e2e.sock}

[ -x "$CTL" ] || { echo "missing horizon-ctl: $CTL" >&2; exit 1; }
[ -x "$GAME" ] || { echo "missing horizon-client: $GAME" >&2; exit 1; }
[ -d "$RES/assets" ] || { echo "missing prepared resources: $RES" >&2; exit 1; }

SCRATCH=$(mktemp -d /tmp/opencode/accdecay-home.XXXXXX)
export HORIZON_HOME=$SCRATCH/game
export HORIZON_LAUNCHER_HOME=$SCRATCH/launcher
export XDG_DATA_HOME=$SCRATCH/data
export XDG_CONFIG_HOME=$SCRATCH/config
export HORIZON_NO_AUDIO=1
mkdir -p "$(dirname "$SOCK")"
rm -f "$SOCK"

ctl() { "$CTL" --socket "$SOCK" "$@"; }

cleanup() {
    rc=$?
    ctl quit >/dev/null 2>&1 || true
    rm -rf "$SCRATCH"
    echo "EXIT:$rc"
}
trap cleanup EXIT

# The game writes worlds to ./saves relative to its working directory, so run
# it from the scratch home: every run then gets a fresh world instead of
# inheriting the last run's blocks and gamerules. A fixed seed keeps the test
# site the same from run to run.
cd "$SCRATCH"

echo "== launch (world AccDecayE2E) =="
"$CTL" --socket "$SOCK" launch \
    --game "$GAME" --world AccDecayE2E --seed 20261102 --creative --headless --wait \
    --resources "$RES"
ctl wait-ready 120

echo "== random ticks off; gamerule =="
ctl command "gamerule random_tick_speed 0"
ctl command "time set noon"

echo "== build a 3-log column, plant ready leaves around it, then fell it =="
ctl command "fill 0 64 0 0 66 0 minecraft:oak_log"
# Stand on the -z side, where no planted leaf covers a face of the column:
# `break` refuses a block whose faces are all hidden ("no face of block ...
# is visible from here"), and the three leaves at z=1 do exactly that from +z.
# A stone step keeps the player at the column's height (a bare `tp` drops him
# into whatever is below and out of sight of the top block); clearing the two
# cells in front of the column keeps the sight line free on any seed.
ctl command "fill -1 64 -2 1 66 -2 minecraft:air"
ctl command "fill 0 64 -1 0 66 -1 minecraft:air"
ctl command "fill -1 63 -2 1 63 -2 minecraft:stone"
ctl command "tp 0 64 -2"
ctl wait 10
# Leaves exist before the break, as in play: the log update and the engine's
# retries then see the same blocks.
for p in "0 64 1" "0 65 1" "0 66 1" "1 65 0" "-1 65 0" "0 67 0"; do
    set -- $p
    ctl command "setblock $1 $2 $3 minecraft:oak_leaves[persistent=false,distance=7]"
done
echo "== control leaf: no felled tree nearby =="
# Clear the control cells first: a generated tree would otherwise make the
# setblock a no-op ("Could not set the block" when the state is unchanged).
ctl command "fill 10 63 0 10 65 0 minecraft:air"
ctl command "setblock 10 64 0 minecraft:oak_leaves[persistent=false,distance=7]"
ctl command "setblock 10 63 0 minecraft:stone"

echo "== fell the column (player breaks -> destroy_block) =="
ctl command "tp 0 64 -2"
T0=$(ctl state | grep -o '"ticks":[0-9]*' | tail -1)
ctl break 0 64 0

echo "== early check: 13 ticks after the first break the leaf must still be there =="
# At this point only the bottom log is gone, so Horizon's distance chain has
# re-read the leaf as distance=2 (it is still next to the two standing logs):
# the engine must retry it, never remove it early. The 20-tick delay itself is
# pinned by the unit test (`first removal exactly 20 ticks after the break`);
# a ctl read lands ~15-19 ticks after the break here (call and chunk-update
# latency), too close to the boundary to time 6 vs 20 ticks in game.
ctl wait 13
EARLY=$(ctl block 0 64 1)
T1=$(ctl state | grep -o '"ticks":[0-9]*' | tail -1)
echo "tick markers: $T0 before the break, $T1 after the read"
echo "leaf at 0,64,1 thirteen ticks after the first break (must still be oak_leaves): $EARLY"
if ! grep -q '"block":"minecraft:oak_leaves"' <<<"$EARLY"; then
    echo "FAIL: a column leaf was removed before the 20-tick delay" >&2
    exit 1
fi

ctl break 0 65 0
ctl break 0 66 0

echo "== wait for the engine (delay 20 ticks, then budgets) =="
ctl wait 60

echo "== report =="
BOX=$(ctl blocks -2 62 -2 2 70 2)
echo "$BOX"
echo "-- control block (must still be oak_leaves):"
CONTROL=$(ctl block 10 64 0)
echo "$CONTROL"

echo "== verdict =="
if grep -q '"minecraft:oak_leaves' <<<"$BOX" || grep -q '"minecraft:oak_log' <<<"$BOX"; then
    echo "FAIL: a leaf or a log is left in the box around the felled column" >&2
    exit 1
fi
if ! grep -q '"block":"minecraft:oak_leaves"' <<<"$CONTROL"; then
    echo "FAIL: the control leaf is gone" >&2
    exit 1
fi

echo "== waterlogged leaf: second felled column on an elevated stone arena =="
# Upstream breaks a leaf into its fluid's legacy block, so a waterlogged leaf
# must leave minecraft:water (not air). The arena is built in the air at y=149
# so no generated terrain can fall into the water or replace it: the water cell
# is enclosed by stone on every side but the felled log's face.
ctl command "fill 36 149 -12 48 149 12 minecraft:stone"
ctl command "fill 36 150 -12 48 172 12 minecraft:air"
ctl command "fill 40 150 6 40 152 6 minecraft:oak_log"
ctl command "setblock 41 150 6 minecraft:stone"
ctl command "setblock 42 151 6 minecraft:stone"
ctl command "setblock 41 152 6 minecraft:stone"
ctl command "setblock 41 151 5 minecraft:stone"
ctl command "setblock 41 151 7 minecraft:stone"
ctl command "setblock 41 151 6 minecraft:oak_leaves[persistent=false,distance=7,waterlogged=true]"
echo "== waterlogged control: same leaf, but no felled column nearby =="
# This leaf proves the water above comes from the break and the engine: with
# random ticks off, an untouched waterlogged leaf stays a leaf (a drain/decay
# would fail this check).
ctl command "setblock 36 150 -8 minecraft:oak_leaves[persistent=false,distance=7,waterlogged=true]"
# A `block` read straight after a `command` sees the pre-command chunk, so let
# a few ticks pass before reading the state back (dry leaves behave the same).
ctl wait 5
WATER_BEFORE=$(ctl block 41 151 6)
echo "waterlogged leaf before the break: $WATER_BEFORE"
if ! grep -q 'waterlogged=true' <<<"$WATER_BEFORE"; then
    echo "FAIL: the waterlogged test leaf was not planted" >&2
    exit 1
fi
WATER_CTL_BEFORE=$(ctl block 36 150 -8)
echo "waterlogged control before the break: $WATER_CTL_BEFORE"
if ! grep -q 'waterlogged=true' <<<"$WATER_CTL_BEFORE"; then
    echo "FAIL: the waterlogged control leaf was not planted" >&2
    exit 1
fi
ctl command "tp 40 150 4"
ctl wait 10
ctl break 40 150 6
ctl break 40 151 6
ctl break 40 152 6
ctl wait 60
WATER=$(ctl block 41 151 6)
echo "waterlogged leaf position after the scan: $WATER"
if ! grep -q '"block":"minecraft:water"' <<<"$WATER"; then
    echo "FAIL: a waterlogged leaf did not become water" >&2
    exit 1
fi
WATER_CTL=$(ctl block 36 150 -8)
echo "waterlogged control after the scan (must still be oak_leaves): $WATER_CTL"
if grep -q '"block":"minecraft:water"' <<<"$WATER_CTL" || ! grep -q '"block":"minecraft:oak_leaves"' <<<"$WATER_CTL"; then
    echo "FAIL: the untouched waterlogged control leaf did not stay a leaf" >&2
    exit 1
fi
echo "PASS: no leaf and no log left in the box; the control leaf is still oak_leaves; the waterlogged leaf became water; the unfelled waterlogged control stayed a leaf"

if [ "${SELFTEST:-0}" = 1 ]; then
    echo "== SELFTEST: a deliberately false assertion must fail the run =="
    GONE=$(ctl block 0 64 1)
    echo "the removed column leaf at 0,64,1 reads $GONE; this run asserts it is still oak_leaves"
    if ! grep -q '"block":"minecraft:oak_leaves"' <<<"$GONE"; then
        echo "FAIL: SELFTEST run: the flipped assertion (removed leaf still present) failed the run, as required" >&2
        exit 1
    fi
    echo "SELFTEST run did not fail: the assertion is vacuous" >&2
    exit 2
fi

ctl quit >/dev/null 2>&1 || true
trap - EXIT
rm -rf "$SCRATCH"
echo "done: inspect the box above; the six column leaves should be gone, the control leaf present"
echo "EXIT:0"
