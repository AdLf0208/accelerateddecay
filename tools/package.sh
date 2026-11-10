#!/usr/bin/env bash
# Package the delivered artifacts from the current sources at the campaign pin:
#  - the `.hmod` in `content/`, built and checked by the pin's own
#    `cargo-horizon-mod` (the same code the registry runs), when content-bearing;
#  - the source-only `.hnmod` in the port root (`tools/package.py`, deterministic),
#    when the bundle has a `mixins/<id>/horizon-mixin.toml`;
#  - for a split content+code port, the built `.hmod` is carried inside the bundle
#    at `mixins/<id>/data/<id>-content.hmod` (the AE2 pattern), so the one
#    `.hnmod` installs both halves.
# Run inside a build slot (it compiles the packaging tool):
#   ports/tools/build-slot.sh run --job <id>-pack --port ports/<id> \
#       --step package -- env CARGO_INCREMENTAL=0 tools/package.sh
set -euo pipefail

PORT=$(cd "$(dirname "$0")/.." && pwd)
CAMPAIGN=$(cd "$PORT/../.." && pwd)
HORIZON=${HORIZON:-$CAMPAIGN/ports/_shared/pins/horizon-1d63e0a}
ID=$(basename "$PORT")
LOGS=$PORT/target/ladder
mkdir -p "$LOGS"
cd "$PORT"

HAVE_CONTENT=0
if [ -f "$PORT/content/horizon-mod.toml" ]; then
    HAVE_CONTENT=1
    CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PORT/target/carry-tools}
    export CARGO_TARGET_DIR
    (cd content && cargo run --locked --offline --quiet \
        --manifest-path "$HORIZON/crates/cargo-horizon-mod/Cargo.toml" -- horizon-mod build) \
        2>&1 | tee "$LOGS/package-hmod.log"
    cp "content/target/horizon-mod/$ID.hmod" "content/$ID.hmod"
    (cd content && cargo run --locked --offline --quiet \
        --manifest-path "$HORIZON/crates/cargo-horizon-mod/Cargo.toml" -- horizon-mod check "$ID.hmod") \
        2>&1 | tee -a "$LOGS/package-hmod.log"
fi

if [ -f "$PORT/mixins/$ID/horizon-mixin.toml" ]; then
    if [ "$HAVE_CONTENT" = 1 ]; then
        mkdir -p "mixins/$ID/data"
        cp "content/$ID.hmod" "mixins/$ID/data/$ID-content.hmod"
    fi
    python3 tools/package.py | tee "$LOGS/package-hnmod.log"
fi

echo "== artifacts =="
for f in "content/$ID.hmod" "$ID.hnmod"; do
    [ -f "$f" ] || continue
    size=$(stat -c %s "$f")
    sha=$(sha256sum "$f" | cut -d' ' -f1)
    entries=$(python3 -c "import sys,zipfile; print(len(zipfile.ZipFile(sys.argv[1]).namelist()))" "$f")
    echo "$f size=$size sha256=$sha entries=$entries"
done
