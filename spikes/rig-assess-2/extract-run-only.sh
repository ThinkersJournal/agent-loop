#!/usr/bin/env bash
# Fork-cost experiment: can rig-agent's sans-I/O `run` module stand alone on rig-core types only?
# Copies rig-agent 0.46.0 `src/run`, drops its unit tests, removes the one variant that reaches rig-memory,
# builds against rig-core alone. Output: vendor/run-only (gitignored).
set -euo pipefail
SRC="${RIG_AGENT_SRC:?path to the unpacked rig-agent-0.46.0 crate}"
OUT="$(dirname "$0")/vendor/run-only"
rm -rf "$OUT" && mkdir -p "$OUT/src"
cp -r "$SRC/src/run" "$OUT/src/run"
find "$OUT/src/run" -name tests.rs -delete
find "$OUT/src/run" -type d -empty -delete
python - "$OUT" <<'PY'
import re, glob, sys
out = sys.argv[1]
for f in glob.glob(out + '/src/run/*.rs'):
    b = open(f, 'rb').read().decode().replace('\r\n', '\n')
    b = re.sub(r'\n#\[cfg\(test\)\]\nmod tests;\n', '\n', b)
    if f.endswith('response.rs'):
        i = b.index("    /// The run's conversation could not be loaded from its memory store,")
        j = b.index('    /// The run exhausted its total model-call budget.')
        b = b[:i] + b[j:]
    open(f, 'w', newline='\n').write(b)
PY
cat > "$OUT/Cargo.toml" <<'T'
[package]
name = "run-only"
version = "0.0.0"
edition = "2024"
publish = false
[workspace]
[dependencies]
rig-core = { version = "0.46.0", default-features = false }
serde = { version = "1", features = ["derive", "rc"] }
serde_json = "1"
thiserror = "2"
http = "1"
tracing = "0.1"
T
echo 'pub mod run;' > "$OUT/src/lib.rs"
( cd "$OUT" && cargo build --offline )
find "$OUT/src" -name '*.rs' | xargs cat | wc -l
