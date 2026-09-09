#!/usr/bin/env bash
# Standalone near-mock verification suite (ported from lisp-rlm's
# verify_near_mock.sh, 2026-09-08). Hermetic: no network, no compiler —
# probe contracts ship precompiled in ../fixtures.
set -u
DIR="$(cd "$(dirname "$0")" && pwd)"
FIX="$DIR/../fixtures"
WASM="$FIX/guestbook.wasm"
# prefer release, fall back to debug — CI runs `cargo test` (debug only)
# and shouldn't need a 6-minute release build just to verify
if [ -x "$DIR/../target/release/near-mock" ]; then
  NM="$DIR/../target/release/near-mock"
else
  NM="$DIR/../target/debug/near-mock"
fi
# portable workdir: $TMPDIR when set (sandboxed macOS / CI deny /tmp), /tmp otherwise
WORK="${TMPDIR:-/tmp}/nmverify.$$"
mkdir -p "$WORK"
pass=0; fail=0
ok()   { pass=$((pass+1)); echo "PASS: $1"; }
bad()  { fail=$((fail+1)); echo "FAIL: $1"; }
check() { if printf '%s' "$3" | grep -q "$2"; then ok "$1"; else bad "$1 | wanted '$2' got: $(printf '%s' "$3" | tr '\n' '|' | head -c 200)"; fi }

echo "== help =="
$NM --help >/dev/null 2>&1 && ok "--help exit 0" || bad "--help exit"

echo "== method-not-found lists exports =="
out=$($NM "$WASM" nosuchmethod '{}' 2>&1)
check "hint header" "Available exports" "$out"
check "lists get_signatures" "get_signatures" "$out"

echo "== --json =="
out=$($NM "$WASM" get_signature_count '{}' --json --state "$WORK/s1.json" 2>/dev/null)
check "outcome ok" '"ok"' "$out"
check "gas" 'gas_burnt_tgas' "$out"
check "storage diff" '"added"' "$out"

echo "== --dry-run does not persist =="
S="$WORK/dry.json"
out=$($NM "$WASM" sign '{"message":"dry probe"}' --dry-run --state "$S" 2>&1)
check "dry-run note" "NOT persisted" "$out"
count=$($NM "$WASM" get_signature_count '{}' --state "$S" 2>/dev/null)
check "count still 0" '📄 0' "$count"

echo "== --now / --advance =="
out=$($NM "$FIX/ts.wasm" _run '{}' --now 1700000000 --advance 60 --state "$WORK/ts.bin" 2>/dev/null)
check "ts = (1700000000+60)e9" "1700000060000000000" "$out"
out=$($NM "$FIX/ts.wasm" _run '{}' --now 1234567890 --state "$WORK/ts2.bin" 2>/dev/null)
check "bare --now" "1234567890000000000" "$out"

echo "== random_seed determinism =="
seedof() { $NM "$FIX/rs.wasm" _run '{}' --now "$1" --state "$2" 2>/dev/null | grep -oE '[0-9a-f]{64}'; }
a=$(seedof 1000 "$WORK/r1.bin"); rm -f "$WORK/r1.bin"
b=$(seedof 1000 "$WORK/r2.bin"); rm -f "$WORK/r2.bin"
c=$(seedof 2000 "$WORK/r3.bin"); rm -f "$WORK/r3.bin"
{ [ -n "$a" ] && [ "$a" = "$b" ]; } && ok "same --now ⇒ same seed" || bad "seed stability: [$a] vs [$b]"
{ [ -n "$c" ] && [ "$a" != "$c" ]; } && ok "diff --now ⇒ diff seed" || bad "seed variation: [$a] vs [$c]"
d=$($NM "$FIX/rs.wasm" _run '{}' --state "$WORK/r4.bin" 2>/dev/null | grep -o 'cafe01' | head -1)
NEAR_MOCK_SEED=cafe01 $NM "$FIX/rs.wasm" _run '{}' --state "$WORK/r4.bin" 2>/dev/null | grep -q cafe01 && ok "NEAR_MOCK_SEED pin" || bad "NEAR_MOCK_SEED pin"

echo "== staking =="
S="$WORK/stake.json"
out=$($NM "$WASM" sign '{"message":"stake probe"}' --staking --state "$S" 2>&1)
check "staking lock note" "staking: locked" "$out"
out=$($NM "$WASM" get_signature_count '{}' --state "$S" --json 2>/dev/null)
check "locked_yocto in json" 'locked_yocto' "$out"

echo "== gas schedule =="
$NM --gas-schedule-help > "$WORK/help2.txt" 2>&1
cat > "$WORK/sched.json" <<'EOF'
{"log_base": 13181732, "log_byte": 19335348, "read_register_base": 24108449, "read_register_byte": 3574166, "storage_has_key_base": 56356995, "storage_has_key_key_byte": 81569, "storage_read_base": 56356995, "storage_read_key_byte": 81569, "storage_read_value_byte": 3574166, "storage_remove_base": 64000000, "storage_remove_key_byte": 90563, "storage_write_base": 64000000, "storage_write_key_byte": 90563, "storage_write_value_byte": 3548576, "trie_node": 2280000000, "trie_walk_nodes": 16, "value_return_base": 4141250, "value_return_byte": 3574166}
EOF
out=$($NM "$WASM" sign '{"message":"sched probe"}' --json --gas-schedule "$WORK/sched.json" --state "$WORK/sch.json" 2>&1)
check "--gas-schedule flag" '"gas_burnt_tgas"' "$out"
echo '{"storage_write_base": "not-a-number"}' > "$WORK/bad.json"
$NM "$WASM" get_signature_count '{}' --gas-schedule "$WORK/bad.json" >/dev/null 2>&1
[ $? -ne 0 ] && ok "invalid schedule rejected (exit 1)" || bad "invalid schedule accepted"

echo "== NEP-297 EVENT_JSON =="
out=$($NM "$FIX/ev.wasm" _run '{}' --state "$WORK/ev1.bin" 2>/dev/null)
check "event banner" "📣 EVENT nep171" "$out"
out=$($NM "$FIX/ev.wasm" _run '{}' --json --state "$WORK/ev2.bin" 2>/dev/null | grep '^JSON')
check "events[] in --json" 'nep171' "$out"

echo "== catch_unwind printer safety =="
out=$($NM "$WASM" sign '{"message":"héllo wörld ünïcode ☕ exile"}' --state "$WORK/u.json" 2>&1); rc=$?
check "unicode sign ok" "Success" "$out"
[ $rc -eq 0 ] && ok "exit 0 with unicode state" || bad "exit $rc with unicode state"

echo "== str= / str!= (precompiled eq fixture) =="
for c in "eq_yes:EQ" "eq_no:NE" "ne_yes:DIFF" "ne_no:SAME"; do
  m=${c%%:*}; want=${c##*:}
  out=$($NM "$FIX/eq.wasm" "$m" '{}' --state "$WORK/$m.bin" 2>/dev/null)
  check "wasm $m → $want" "📄 $want" "$out"
done

echo "== scenario: per-step as/predecessor + now/advance =="
cp "$FIX/scen.wasm" "$WORK/contract.wasm"
cat > "$WORK/scen.json" <<'EOF'
{"name": "verify: identity + time travel",
 "steps": [
   {"method": "whoami", "expect": "owner.test.near"},
   {"method": "whoami", "as": "alice.test.near", "expect": "alice.test.near"},
   {"method": "whoami", "as": "bob.test.near", "predecessor": "vault.test.near", "expect": "vault.test.near"},
   {"method": "clock", "now": 1700000000, "expect": "1700000000000000000"},
   {"method": "clock", "advance": 60, "expect": "1700000060000000000"},
   {"method": "clock", "expect": "1700000060000000000"},
   {"method": "gate", "as": "alice.test.near", "expect": "allowed"},
   {"method": "gate", "as": "eve.test.near", "expect": "trap"}
 ]}
EOF
out=$(cd "$WORK" && $NM scenario scen.json 2>&1); rc=$?
check "scenario runner banner" "scenario: verify: identity + time travel" "$out"
check "per-step as honored" "👤 as alice" "$out"
check "per-step predecessor honored" "predecessor vault" "$out"
check "step now" "1700000000000000000" "$out"
check "advance accumulates + persists" "1700000060000000000" "$out"
check "caller gate allowed" "allowed" "$out"
check "caller gate traps for eve" "trap as expected" "$out"
[ $rc -eq 0 ] && ok "scenario exit 0 (9/9)" || bad "scenario exit $rc"
[ -f "$WORK/state.bin" ] && ok "scenario persisted state.bin" || bad "scenario state.bin missing"

# ---- --trace: host-call timeline + per-host gas attribution ----
tout=$(cd "$WORK" && NEAR_MOCK_TRACE=1 $NM scenario scen.json 2>&1)
check "trace timeline entries" "] predecessor_account_id" "$tout"
check "trace per-call gas" "gas=" "$tout"
check "trace per-step summary" "🔍 host trace —" "$tout"
check "trace records predecessor_account_id" "predecessor_account_id" "$tout"
single=$(cd "$WORK" && $NM contract.wasm gate --trace 2>&1)
check "single-call trace summary" "🔍 host trace —" "$single"
check "single-call trace shows storage_write cost" "storage_write" "$single"
jtrace=$(cd "$WORK" && $NM contract.wasm gate --trace --json 2>&1 | grep '^JSON ' | tail -1)
check "json host_trace field" '"host_trace"' "$jtrace"
check "json host_trace totals carry calls+gas" '"calls"' "$jtrace"

echo "== stub-kill surface (2026-09-08) =="
out=$($NM --gas-schedule-help)
check "ecrecover_base pin" "ecrecover_base" "$out"
check "p256_verify_base pin" "p256_verify_base" "$out"
check "alt_bn128_pairing_check_base pin" "alt_bn128_pairing_check_base" "$out"
check "validator_stake_base pin" "validator_stake_base" "$out"
out=$($NM snapshot 2>&1); rc=$?
[ $rc -ne 0 ] && ok "snapshot without args exits nonzero" || bad "snapshot no-arg exit 0"
check "snapshot usage line" "usage: near-mock snapshot" "$out"
out=$($NM snapshot a b --rpc 2>&1); [ $? -ne 0 ] && ok "malformed snapshot exits nonzero" || bad "malformed snapshot exit 0"
out=$($NM --help 2>&1)
check "help lists snapshot" "near-mock snapshot" "$out"
out=$($NM state 2>&1)
check "state import usage" "state import" "$out"

echo "== CLI contract: exit codes / dump JSON / round-trip =="
# #1: contract failure => nonzero exit (the CI pattern: near-mock ... || exit 1)
$NM "$WASM" sign '{}' --state "$WORK/ex1.bin" >/dev/null 2>&1
[ $? -ne 0 ] && ok "trap exits nonzero" || bad "trap exit 0"
$NM "$WASM" sign '{"message":"x"}' --prepaid 0.0001 --state "$WORK/ex2.bin" >/dev/null 2>&1
[ $? -ne 0 ] && ok "out-of-gas exits nonzero" || bad "out-of-gas exit 0"
$NM "$WASM" sign '{"message":"ok"}' --state "$WORK/ex3.bin" >/dev/null 2>&1
[ $? -eq 0 ] && ok "successful call exits 0" || bad "successful call nonzero"
# #2: dump stdout is pure JSON; summary on stderr
$NM state dump "$WORK/ex3.bin" 2>"$WORK/dump.err" | jq -e . >/dev/null 2>&1 \
  && ok "dump stdout parses as JSON" || bad "dump stdout not JSON"
grep -q "keys" "$WORK/dump.err" && ok "dump summary on stderr" || bad "dump summary not on stderr"
# #3: dump | import round-trip (both shapes)
$NM state dump "$WORK/ex3.bin" 2>/dev/null | $NM state import "$WORK/ex4.bin" - >/dev/null 2>&1 \
  && ok "dump|import round-trip (canonical)" || bad "round-trip canonical"
printf '[{"account":"rt.test.near","key":"a2V5","value":"dg=="}]' > "$WORK/legacy.json"
$NM state import "$WORK/ex5.bin" "$WORK/legacy.json" >/dev/null 2>&1 \
  && ok "legacy flat-row import accepted" || bad "legacy import rejected"
# #4: missing contract names the file + the convention
mkdir -p "$WORK/empty" && printf '{"steps":[{"method":"get"}]}' > "$WORK/empty/s.json"
out=$(cd "$WORK/empty" && $NM scenario s.json 2>&1)
check "missing wasm names file" "cannot read contract \`contract.wasm\`" "$out"
check "missing wasm names manifest escape" "override with" "$out"
# C5: omitted args-json must default to {}, not feed the first flag to the contract
out=$($NM "$WASM" get_signatures --view --state "$WORK/c5a.bin" 2>&1)
if printf '%s' "$out" | grep -q "deserialize input"; then bad "single-call C5: flag fed as args"; else ok "single-call C5: omitted args-json defaults to {}"; fi
out=$($NM cross "$WORK/c5b.bin" "gb=$WASM" gb.test.near get_signatures --view 2>&1)
if printf '%s' "$out" | grep -q "deserialize input"; then bad "cross C5: flag fed as args"; else ok "cross C5: omitted args-json defaults to {}"; fi

# --version / -V: version string from Cargo.toml, exit 0
out=$($NM --version 2>&1); rc=$?
check "--version prints pkg version" "near-mock 0\." "$out"
[ $rc -eq 0 ] && ok "--version exit 0" || bad "--version exit $rc"
out=$($NM -V 2>&1)
check "-V alias works" "near-mock 0\." "$out"

# storage namespace: single-call state lands under the DOCUMENTED default
# account (help: "NEAR_MOCK_CONTRACT default escrow.test.near"), matching
# what current_account_id() reports — not the "" partition (dump showed
# account:"", the escrow prefix filter returned 0 rows, and cross/scenario
# couldn't see single-call state).
$NM "$WASM" sign '{"message":"ns interop"}' --state "$WORK/ns.bin" >/dev/null 2>&1
out=$($NM state dump "$WORK/ns.bin" 2>/dev/null)
check "single-call state under escrow.test.near" '"account": "escrow.test.near"' "$out"
out=$($NM state dump "$WORK/ns.bin" escrow.test.near 2>/dev/null | jq -e 'length == 1' >/dev/null 2>&1 && echo FILTEROK)
check "dump prefix filter finds default account" "FILTEROK" "$out"
# interop: cross under the same account reads single-call state
out=$($NM cross "$WORK/ns.bin" "escrow.test.near=$WASM" escrow.test.near get_signature_count 2>/dev/null)
check "cross reads single-call state (same account)" "📄 1" "$out"

# skill ships with the binary (0.1.6): frontmatter present, command works
out=$($NM skill --stdout 2>/dev/null | head -2 | tail -1)
check "skill --stdout carries frontmatter name" "name: near-mock" "$out"

# shellcheck disable=SC2312
SKILLTEST="$WORK/skillproj"; mkdir -p "$SKILLTEST"
out=$(cd "$SKILLTEST" && $NM skill 2>&1)
check "skill installs into project" "SKILL.md" "$out"
[ -f "$SKILLTEST/.agents/skills/near-mock/example-scenario.json" ] \
  && ok "skill scenario example included" || bad "scenario example missing"

# shellcheck disable=SC2312
out=$(cd "$SKILLTEST" && $NM skill 2>&1)
check "skill skip-if-exists" "already present" "$out"

# shellcheck disable=SC2312
out=$(cd "$SKILLTEST" && $NM skill --force 2>&1)
check "skill --force overwrites" "✅" "$out"

echo
echo "RESULT: $pass passed, $fail failed"
[ $fail -eq 0 ]
