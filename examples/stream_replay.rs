//! stream_replay v2 — MULTI-CONTRACT live replay: mainnet → near-mock.
//!
//! Usage:
//!   cargo run --release --example stream_replay -- \
//!       --manifest /tmp/replay-wasms/manifest.json \
//!       [--wasm acct=path]... [--init acct=json]... \
//!       [--max TXS] [--forever] [--poll-secs 30] [--verbose] [--errors-only] \
//!       [--fail-dir /tmp/replay-failures] [--cursor /tmp/stream_replay_cursor.json]
//!
//! Live UI: TTY → one-line ticker (uptime, rate, poll cadence, totals,
//! last receipt); redirected → the same line every ~5s. --verbose adds a
//! per-receipt feed line (class glyphs: ✓=exact ✓ok ✗=class ✗fail ↓mock-fail
//! ↑mock-only-ok ⟳upgrade? 🚨infra).
//! --errors-only: dead silent except 🚨 INFRA alerts, fetch errors, failed
//! inits, and the final findings line — for unattended runs.
//!
//! One MockChain, N contracts (per-account storage partitions), fan-out:
//!   - poll every tracked account (/v0/account), merge + dedupe tx hashes
//!   - per tx, replay ONLY the FIRST receipt whose receiver is deployed AND
//!     whose predecessor is NOT (a real entry point). Downstream receipts to
//!     our set are DAG-covered: the promise machinery executes them against
//!     the REAL deployed wasms (cross-contract + self-callbacks with real
//!     promise_results — the tier single-contract mode had to skip).
//!   - state stays in RAM; disk = fixtures (only on findings) + cursor.
//!
//! Classification (fresh-state Mode A+B):
//!   both-ok / both-fail (+ same-class panic) / mock↓mn↑ / mock↑mn↓ /
//!   log-exact (byte-identical logs) / INFRA (fire() Err — the real hunt) /
//!   upgrade-suspect (method vanished → contract was upgraded).

use near_mock::chain::MockChain;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

const API: &str = "https://tx.main.fastnear.com";
const DEFAULT_MANIFEST: &str = "/tmp/replay-wasms/manifest.json";

// ───────────────────────── curl transport ─────────────────────────

fn post_json(url: &str, body: &Value) -> Result<Value, String> {
    let mut last_err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2u64 << (attempt - 1)));
        }
        let out = std::process::Command::new("curl")
            .args([
                "-s",
                "--max-time",
                "60",
                "--retry",
                "5",
                "--retry-all-errors",
                "--retry-connrefused",
                "--retry-max-time",
                "300",
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "-d",
                &body.to_string(),
                url,
            ])
            .output()
            .map_err(|e| format!("curl spawn: {e}"))?;
        if !out.status.success() {
            last_err = format!("curl exit {}", out.status);
            continue;
        }
        match serde_json::from_slice::<Value>(&out.stdout) {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = format!(
                    "bad JSON: {e} (body starts {:?})",
                    String::from_utf8_lossy(&out.stdout[..out.stdout.len().min(60)])
                )
            }
        }
    }
    Err(last_err)
}

// ───────────────────────── method interning (call() wants &'static str) ─────────────────────────

fn intern(s: &str) -> &'static str {
    static CACHE: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);
    let mut g = CACHE.lock().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(leaked) = map.get(s) {
        return leaked;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    map.insert(s.to_string(), leaked);
    leaked
}

// ───────────────────────── scoreboard ─────────────────────────

#[derive(Default, Clone)]
struct MethodStat {
    n: u64,
    both_ok: u64,
    both_fail: u64,
    mock_fail_mainnet_ok: u64,
    mock_ok_mainnet_fail: u64,
    log_exact: u64,
    gas_n: u64,  // gas compared (both-ok receipts with mn_gas>0)
    gas_ok: u64, // ...and within the tolerance band
    same_class_fail: u64,
    chain_n: u64,          // receipts compared with a >1-receipt subtree fold
    chain_fail_exact: u64, // subtree failures == mock receipt_failures (order)
    infra: u64,
    upgrade_suspect: u64,
}

#[derive(Default)]
struct Score {
    // "contract::method" → stats
    methods: BTreeMap<String, MethodStat>,
    internal_covered: u64, // receiver∈set & predecessor∈set → DAG-covered, skipped
    other_receiver: u64,
    data_receipt: u64,
    transfer_only: u64,
    multi_fc: u64,
    non_utf8_args: u64,
    parse_surprise: u64,
    fetch_err: u64,
    txs: u64,
    receipts: u64,
}

impl Score {
    fn print(&self, contracts: &[String]) {
        println!(
            "\n┌─ scoreboard ─ txs={} receipts={} skips(internal={} other-recv={} data={} xfer={} multi-fc={} bad-args={} parse={} fetch-err={})",
            self.txs,
            self.receipts,
            self.internal_covered,
            self.other_receiver,
            self.data_receipt,
            self.transfer_only,
            self.multi_fc,
            self.non_utf8_args,
            self.parse_surprise,
            self.fetch_err
        );
        for c in contracts {
            let prefix = format!("{c}::");
            let mut rows: Vec<(&String, &MethodStat)> = self
                .methods
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .collect();
            if rows.is_empty() {
                continue;
            }
            let tot: u64 = rows.iter().map(|(_, s)| s.n).sum();
            let folded: u64 = rows.iter().map(|(_, s)| s.chain_n).sum();
            println!("│ ── {c} ({tot} receipts, {folded} folded-subtree)");
            rows.sort_by_key(|(_, s)| std::cmp::Reverse(s.n));
            println!(
                "│ {:<34} {:>6} {:>7} {:>9} {:>8} {:>8} {:>9} {:>10} {:>12} {:>8} {:>6} {:>5}",
                "method",
                "n",
                "both-ok",
                "both-fail",
                "mn↓↑",
                "mn↑↓",
                "log-exact",
                "same-class",
                "chain-exact",
                "gas-ok",
                "upgrd",
                "infra"
            );
            for (k, s) in rows.iter().take(25) {
                let m = k.strip_prefix(&prefix).unwrap_or(k);
                println!(
                    "│ {:<34} {:>6} {:>7} {:>9} {:>8} {:>8} {:>9} {:>10} {:>12} {:>8} {:>6} {:>5}",
                    m,
                    s.n,
                    s.both_ok,
                    s.both_fail,
                    s.mock_fail_mainnet_ok,
                    s.mock_ok_mainnet_fail,
                    s.log_exact,
                    s.same_class_fail,
                    format!("{}/{}", s.chain_fail_exact, s.chain_n),
                    format!("{}/{}", s.gas_ok, s.gas_n),
                    s.upgrade_suspect,
                    s.infra
                );
            }
        }
        println!("└─");
    }
}

// ───────────────────────── fixture dump ─────────────────────────

fn save_fixture(dir: &str, class: &str, tx_hash: &str, receipt_id: &str, payload: &Value) {
    let _ = std::fs::create_dir_all(dir);
    let name = format!(
        "{class}_{}_{}.json",
        &tx_hash[..11.min(tx_hash.len())],
        &receipt_id[..11.min(receipt_id.len())]
    );
    match serde_json::to_string_pretty(payload) {
        Ok(s) => {
            std::fs::write(format!("{dir}/{name}"), s).ok();
        }
        Err(_) => {
            std::fs::write(format!("{dir}/{name}.raw"), payload.to_string()).ok();
        }
    }
}

// ───────────────────────── mainnet receipt model ─────────────────────────

struct ReplayTarget {
    receiver: String,
    method: String,
    args_b64: String,
    deposit: u128,
    predecessor: String,
    receipt_id: String,
    block_height: u64,
    receipt_index: u64,
    block_ts_nanos: u128,
    mn_success: bool,
    mn_failure: Option<String>,
    mn_logs: Vec<String>,
    mn_gas: u64, // entry receipt gas_burnt (action fees + instr + hosts)
    // ── subtree fold ──
    // The mock executes the whole promise DAG inside one call, so the honest
    // comparison unit is the FOLD of mainnet's receipt subtree (entry +
    // descendants via outcome.receipt_ids), not the entry receipt alone.
    // sub_size==1 (no promises) ⇒ fold ≡ entry data.
    sub_size: usize,
    sub_logs: Vec<String>,
    sub_failures: Vec<String>,
}

enum Extract {
    Replay(ReplayTarget),
    SkipInternal, // receiver ∈ set, predecessor ∈ set → DAG-covered
    SkipOther,    // receiver ∉ set
    SkipData,     // Data receipt / data-dependent callback
    SkipTransfer, // no FunctionCall action
    SkipMultiFc,
    ParseErr(&'static str),
}

fn extract(receipt_entry: &Value, deployed: &HashSet<&str>) -> Extract {
    let inner = match receipt_entry.get("receipt") {
        Some(i) => i,
        None => return Extract::ParseErr("no .receipt"),
    };
    let receiver = inner
        .get("receiver_id")
        .and_then(|r| r.as_str())
        .unwrap_or("");
    if !deployed.contains(receiver) {
        return Extract::SkipOther;
    }
    let predecessor = inner
        .get("predecessor_id")
        .and_then(|p| p.as_str())
        .unwrap_or("");
    if deployed.contains(predecessor) {
        // internal to our set → covered by the promise DAG of the entry replay
        return Extract::SkipInternal;
    }
    let action = match inner.get("receipt").and_then(|r| r.get("Action")) {
        Some(a) => a,
        None => return Extract::SkipData, // {"Data": ..}
    };
    if action
        .get("input_data_ids")
        .and_then(|d| d.as_array())
        .map(|d| !d.is_empty())
        .unwrap_or(false)
    {
        return Extract::SkipData; // callback awaiting promise results
    }
    let fcs: Vec<&Value> = action
        .get("actions")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.get("FunctionCall")).collect())
        .unwrap_or_default();
    if fcs.is_empty() {
        return Extract::SkipTransfer;
    }
    if fcs.len() > 1 {
        return Extract::SkipMultiFc;
    }
    let fc = fcs[0];
    let method = match fc.get("method_name").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => return Extract::ParseErr("no-method"),
    };
    let args_b64 = match fc.get("args").and_then(|a| a.as_str()) {
        Some(a) => a.to_string(),
        None => return Extract::ParseErr("no-args"),
    };
    let deposit = fc
        .get("deposit")
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse().ok())
        .unwrap_or(0);
    let receipt_id = match inner.get("receipt_id").and_then(|r| r.as_str()) {
        Some(r) => r.to_string(),
        None => return Extract::ParseErr("no-rid"),
    };
    let block_height = inner
        .get("block_height")
        .and_then(|b| b.as_u64())
        .unwrap_or(0);
    let receipt_index = inner
        .get("receipt_index")
        .and_then(|i| i.as_u64())
        .unwrap_or(0);
    let block_ts_nanos = inner
        .get("block_timestamp")
        .and_then(|t| {
            t.as_u64()
                .or_else(|| t.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0) as u128;

    let eo = match receipt_entry
        .get("execution_outcome")
        .and_then(|e| e.get("outcome"))
    {
        Some(e) => e,
        None => return Extract::ParseErr("no-outcome"),
    };
    let status = eo.get("status").cloned().unwrap_or(json!(null));
    let mn_failure = status
        .pointer("/Failure/ActionError/kind/FunctionCallError/ExecutionError")
        .and_then(|e| e.as_str())
        .map(String::from)
        .or_else(|| status.get("Failure").map(|f| f.to_string()));
    let mn_success = mn_failure.is_none();
    let mn_logs = eo
        .get("logs")
        .and_then(|l| l.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mn_gas = eo.get("gas_burnt").and_then(|g| g.as_u64()).unwrap_or(0);
    Extract::Replay(ReplayTarget {
        receiver: receiver.to_string(),
        method,
        args_b64,
        deposit,
        predecessor: predecessor.to_string(),
        receipt_id,
        block_height,
        receipt_index,
        block_ts_nanos,
        mn_success,
        mn_failure: mn_failure.clone(),
        mn_logs,
        mn_gas,
        sub_size: 1,
        sub_logs: Vec::new(),
        sub_failures: mn_failure.into_iter().collect(),
    })
}

fn u64_at(v: &Value, pointer: &str) -> u64 {
    v.pointer(pointer)
        .and_then(|x| {
            x.as_u64()
                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0)
}

/// Fold the mainnet receipt SUBTREE rooted at the entry receipt into the
/// comparison unit the mock produces: logs concatenated in execution order
/// (block_height, outcome index), failures (ExecutionError strings) in the
/// same order. BFS via each outcome's receipt_ids (children it spawned).
fn fold_subtree(entry: &Value, by_id: &HashMap<&str, &Value>) -> (usize, Vec<String>, Vec<String>) {
    let mut ordered: Vec<(u64, u64, &Value)> = Vec::new();
    let mut queue = std::collections::VecDeque::new();
    let mut visited: HashSet<&str> = HashSet::new();
    queue.push_back(entry);
    while let Some(r) = queue.pop_front() {
        let id = r
            .pointer("/receipt/receipt_id")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if id.is_empty() || !visited.insert(id) {
            continue;
        }
        ordered.push((
            u64_at(r, "/execution_outcome/block_height"),
            u64_at(r, "/execution_outcome/index"),
            r,
        ));
        if let Some(kids) = r
            .pointer("/execution_outcome/outcome/receipt_ids")
            .and_then(|x| x.as_array())
        {
            for k in kids {
                if let Some(kr) = k.as_str().and_then(|kid| by_id.get(kid)) {
                    queue.push_back(*kr);
                }
            }
        }
    }
    ordered.sort_by_key(|(h, i, _)| (*h, *i));
    let mut logs = Vec::new();
    let mut failures = Vec::new();
    for (_, _, r) in &ordered {
        let Some(eo) = r.pointer("/execution_outcome/outcome") else {
            continue;
        };
        if let Some(l) = eo.get("logs").and_then(|x| x.as_array()) {
            logs.extend(l.iter().filter_map(|s| s.as_str().map(String::from)));
        }
        if let Some(f) = eo
            .pointer("/status/Failure/ActionError/kind/FunctionCallError/ExecutionError")
            .and_then(|x| x.as_str())
        {
            failures.push(f.to_string());
        }
    }
    (ordered.len(), logs, failures)
}

// ───────────────────────── flags ─────────────────────────

struct Cfg {
    accounts: Vec<(String, String)>,          // (account, wasm path)
    inits: HashMap<String, (String, String)>, // account → (init method, args JSON)
    max_txs: u64,
    poll_secs: u64,
    fail_dir: String,
    cursor_path: String,
    verbose: bool,     // per-receipt feed lines
    errors_only: bool, // --errors-only: silence ticker/scoreboards/banner; alerts + failures only
    gas_band: f64,     // gas tolerance (mock vs mainnet), fraction — 0.05 = ±5%
}

fn parse_flags() -> Cfg {
    let args: Vec<String> = std::env::args().collect();
    let val = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let forever = args.iter().any(|a| a == "--forever");

    let mut accounts: Vec<(String, String)> = Vec::new();
    // --manifest first (bulk), then --wasm overrides/adds
    let manifest_path = val("--manifest").unwrap_or_else(|| DEFAULT_MANIFEST.to_string());
    if let Ok(m) = std::fs::read_to_string(&manifest_path) {
        if let Ok(v) = serde_json::from_str::<Value>(&m) {
            if let Some(obj) = v.as_object() {
                for (acct, info) in obj {
                    if let Some(path) = info.get("path").and_then(|p| p.as_str()) {
                        accounts.push((acct.clone(), path.to_string()));
                    }
                }
            }
        }
    }
    let mut i = 0;
    while i + 1 < args.len() {
        if args[i] == "--wasm" {
            if let Some((acct, path)) = args[i + 1].split_once('=') {
                accounts.retain(|(a, _)| a != acct);
                accounts.push((acct.to_string(), path.to_string()));
            }
        }
        i += 1;
    }
    accounts.sort_by(|a, b| a.0.cmp(&b.0));

    let mut inits: HashMap<String, (String, String)> = HashMap::new();
    let mut i = 0;
    while i + 1 < args.len() {
        if args[i] == "--init" {
            // --init acct=method:json   (method optional, default "new")
            if let Some((acct, rest)) = args[i + 1].split_once('=') {
                let (method, j) = rest.split_once(':').unwrap_or(("new", rest));
                inits.insert(acct.to_string(), (method.to_string(), j.to_string()));
            }
        }
        i += 1;
    }

    // Fresh-deploy init args, discovered against the live contracts (a shape
    // that mainnet rejects with "already initialized" parses correctly).
    // Sources: field-walk via deserialize errors; omft's from its original
    // deployment tx (2GVTvFxGjL6PpgGf8vr3bEg4yk27cyMmyaUR5XBErRQE).
    let default_inits: &[(&str, &str, &str)] = &[
        (
            "intents.near",
            "new",
            r#"{"config":{"wnear_id":"wrap.near","fees":{"fee":0,"fee_collector":"intents.near"},"roles":{"super_admins":["intents.near"],"grantees":{"DAO":["intents.near"]}}}}"#,
        ),
        ("wrap.near", "new", "{}"),
        // aurora: raw-wasm engine, borsh args — init skipped Mode A (original
        // deploy tx on mainnet is the authoritative source if needed later)
        (
            "v2.ref-finance.near",
            "new",
            r#"{"owner_id":"v2.ref-finance.near","boost_farm_id":"boost.ref-finance.near","burrowland_id":"borrowing.burrowland.near","exchange_fee":25,"referral_fee":25}"#,
        ),
        (
            "token.sweat",
            "new",
            r#"{"holding_account_id":"holding.sweat","super_admin_account_id":"token.sweat","oracle_account_ids":["token.sweat"],"denylist_manager_account_ids":["token.sweat"],"pause_manager_account_ids":["token.sweat"],"unpause_manager_account_ids":["token.sweat"]}"#,
        ),
        (
            "omft.near",
            "new",
            r#"{"super_admins":["omft.near"],"admins":{},"grantees":{"DAO":["omft.near"],"TokenDeployer":["omft.near","defuse-bridge-mng1.near"],"TokenDepositer":["omft.near","bridge-mng.near","defuse-bridge-mng1.near"]}}"#,
        ),
        (
            "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1",
            "init",
            r#"{"admin_ids":["intents.near"],"master_minter_ids":["intents.near"],"owner_ids":["intents.near"],"pauser_ids":["intents.near"],"blocklister_id":"intents.near","metadata":{"spec":"ft-1.0.0","name":"USDC","symbol":"USDC","decimals":6}}"#,
        ),
    ];
    for (acct, method, init_args) in default_inits {
        inits
            .entry(acct.to_string())
            .or_insert_with(|| (method.to_string(), init_args.to_string()));
    }

    Cfg {
        accounts,
        inits,
        max_txs: if forever {
            u64::MAX
        } else {
            val("--max").and_then(|v| v.parse().ok()).unwrap_or(150)
        },
        poll_secs: val("--poll-secs")
            .and_then(|v| v.parse().ok())
            .unwrap_or(30),
        fail_dir: val("--fail-dir").unwrap_or_else(|| "/tmp/replay-failures".to_string()),
        cursor_path: val("--cursor")
            .unwrap_or_else(|| "/tmp/stream_replay_cursor.json".to_string()),
        verbose: args.iter().any(|a| a == "--verbose")
            && !args.iter().any(|a| a == "--errors-only"),
        errors_only: args.iter().any(|a| a == "--errors-only"),
        gas_band: val("--gas-band")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.05),
    }
}

// ───────────────────── live status ticker ─────────────────────
// TTY: single line redrawn in place (\r). File/pipe: one line per ~5s so
// overnight logs show liveness without flooding (≈ 700 lines/day at 5s).
struct Ticker {
    start: std::time::Instant,
    receipts: std::collections::VecDeque<std::time::Instant>, // rolling 60s window
    last_draw: std::time::Instant,
    is_tty: bool,
    polls: u64,
    last_poll_new: u64,
    last_line: usize,
    last_action: String,
    head_block: u64,
    enabled: bool, // false under --errors-only
}

impl Ticker {
    fn new() -> Self {
        Self {
            start: std::time::Instant::now(),
            receipts: Default::default(),
            last_draw: std::time::Instant::now(),
            is_tty: std::io::IsTerminal::is_terminal(&std::io::stdout()),
            polls: 0,
            last_poll_new: 0,
            last_line: 0,
            last_action: "starting…".into(),
            head_block: 0,
            enabled: true,
        }
    }

    fn note_receipt(&mut self) {
        let now = std::time::Instant::now();
        self.receipts.push_back(now);
        while self
            .receipts
            .front()
            .is_some_and(|t| t.elapsed().as_secs() > 60)
        {
            self.receipts.pop_front();
        }
    }

    fn note_poll(&mut self, new_txs: u64) {
        self.polls += 1;
        self.last_poll_new = new_txs;
    }

    fn note_action(&mut self, contract: &str, method: &str, class: &str, block: u64) {
        let c: String = contract.chars().take(14).collect();
        self.last_action = format!("{c}::{method} {class}");
        if block > self.head_block {
            self.head_block = block;
        }
    }

    fn draw(&mut self, score: &Score) {
        if !self.enabled {
            return;
        }
        let interval = if self.is_tty {
            std::time::Duration::from_millis(250)
        } else {
            std::time::Duration::from_secs(5)
        };
        if self.last_draw.elapsed() < interval {
            return;
        }
        self.last_draw = std::time::Instant::now();
        let up = self.start.elapsed().as_secs();
        let up = if up >= 3600 {
            format!("{}:{:02}:{:02}", up / 3600, (up % 3600) / 60, up % 60)
        } else {
            format!("{:02}:{:02}", up / 60, up % 60)
        };
        let (ok, exact, fail, same, infra, upg) = (
            score.methods.values().map(|m| m.both_ok).sum::<u64>(),
            score.methods.values().map(|m| m.log_exact).sum::<u64>(),
            score.methods.values().map(|m| m.both_fail).sum::<u64>(),
            score
                .methods
                .values()
                .map(|m| m.same_class_fail)
                .sum::<u64>(),
            score.methods.values().map(|m| m.infra).sum::<u64>(),
            score
                .methods
                .values()
                .map(|m| m.upgrade_suspect)
                .sum::<u64>(),
        );
        let skips = score.internal_covered
            + score.other_receiver
            + score.transfer_only
            + score.non_utf8_args;
        let line = format!(
            "⏱ {up} │ txs {} │ rcp {} ({}/min) │ poll #{} +{}tx @{} │ ok {} (={}ex) fail {} (={}) │ skips {} │ 🚨{}↑{} │ last: {}",
            score.txs,
            score.receipts,
            self.receipts.len(),
            self.polls,
            self.last_poll_new,
            self.head_block,
            ok, exact, fail, same,
            skips,
            infra, upg,
            self.last_action
        );
        if self.is_tty {
            use std::io::Write;
            let pad = self.last_line.saturating_sub(line.len());
            print!("\r{line}{}", " ".repeat(pad));
            let _ = std::io::stdout().flush();
            self.last_line = line.len();
        } else {
            println!("{line}");
        }
    }

    /// newline if a \r line is live (before scoreboards / exit).
    fn finish(&self) {
        if self.is_tty && self.enabled {
            println!();
        }
    }
}

// ───────────────────────── cursor (per account) ─────────────────────────

type Cursors = HashMap<String, (u64, u64)>; // account → (block, index)

fn load_cursors(path: &str) -> Cursors {
    let v = match std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    {
        Some(v) => v,
        None => return Cursors::new(),
    };
    let mut out = Cursors::new();
    if let Some(obj) = v.get("accounts").and_then(|a| a.as_object()) {
        for (acct, c) in obj {
            let b = c.get("block").and_then(|x| x.as_u64()).unwrap_or(0);
            let i = c.get("index").and_then(|x| x.as_u64()).unwrap_or(0);
            out.insert(acct.clone(), (b, i));
        }
    }
    out
}

fn save_cursors(path: &str, cursors: &Cursors) {
    let mut m = serde_json::Map::new();
    for (acct, (b, i)) in cursors {
        m.insert(acct.clone(), json!({"block": b, "index": i}));
    }
    std::fs::write(
        path,
        serde_json::to_string(
            &json!({"accounts": m, "updated": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()}),
        )
        .unwrap(),
    )
    .ok();
}

// ───────────────────────── main ─────────────────────────

fn main() {
    std::env::set_var("NEAR_MOCK_QUIET", "1"); // capture still lands in CallOutcome.logs
    let cfg = parse_flags();
    if cfg.accounts.is_empty() {
        eprintln!(
            "no contracts (manifest {} + --wasm flags)",
            DEFAULT_MANIFEST
        );
        std::process::exit(1);
    }

    if !cfg.errors_only {
        println!("tracking {} contracts:", cfg.accounts.len());
    }
    let mut builder = MockChain::builder().signer("intents.near");
    for (acct, path) in &cfg.accounts {
        let wasm = std::fs::read(path).unwrap_or_else(|e| {
            eprintln!("cannot read {path}: {e}");
            std::process::exit(1);
        });
        if !cfg.errors_only {
            println!("  {acct}  {} bytes", wasm.len());
        }
        // contract account ids are runtime Strings here; leak for 'static
        builder = builder.contract_bytes(Box::leak(acct.clone().into_boxed_str()), wasm);
    }
    // Clock: base = epoch; the loop advances to each receipt's block ts.
    let chain = builder.now(0).build().expect("chain build");

    // optional per-contract init (--init acct=json overrides defaults).
    // Init defaults come from parse_flags (per-contract, discovered shapes).
    // Fire-and-report; a failed init is a finding, not a crash.
    for (acct, (method, init_args)) in &cfg.inits {
        if let Some((a, _)) = cfg.accounts.iter().find(|(a, _)| a == acct) {
            let acct_static: &str = Box::leak(a.clone().into_boxed_str());
            let out = chain
                .call(acct_static, intern(method))
                .args(init_args.clone())
                .fire();
            match out {
                Ok(o) if o.ok => {
                    if !cfg.errors_only {
                        println!("  init {acct}: ok");
                    }
                }
                other => println!("  init {acct}: FAILED {other:?} (receipts will show it)"),
            }
        }
    }

    let deployed: HashSet<&str> = cfg.accounts.iter().map(|(a, _)| a.as_str()).collect();
    let accounts: Vec<String> = cfg.accounts.iter().map(|(a, _)| a.clone()).collect();

    let mut score = Score::default();
    let mut processed: HashSet<String> = HashSet::new();
    let mut cursors = load_cursors(&cfg.cursor_path);
    let mut last_ts_secs: Option<i64> = None;
    let mut consecutive_fetch_failures = 0u32;
    let mut ticker = Ticker::new();
    ticker.enabled = !cfg.errors_only;

    'outer: loop {
        // ── discover: poll every account, merge + dedupe tx metas ──
        let mut fresh: Vec<(u64, u64, String, String)> = Vec::new(); // (block, index, hash, account)
        for acct in &accounts {
            let (cur_b, cur_i) = cursors.get(acct).copied().unwrap_or((0, 0));
            let page = match post_json(
                &format!("{API}/v0/account"),
                &json!({"account_id": acct, "is_receiver": true, "limit": 100, "desc": true,
                        "from_tx_block_height": cur_b}),
            ) {
                Ok(v) => v,
                Err(e) => {
                    score.fetch_err += 1;
                    consecutive_fetch_failures += 1;
                    println!("fetch error on {acct} ({consecutive_fetch_failures}): {e}");
                    if consecutive_fetch_failures > 30 {
                        std::process::exit(1);
                    }
                    continue;
                }
            };
            consecutive_fetch_failures = 0;
            for t in page
                .get("account_txs")
                .and_then(|a| a.as_array())
                .unwrap_or(&vec![])
            {
                let h = t
                    .get("tx_block_height")
                    .and_then(|b| b.as_u64())
                    .unwrap_or(0);
                let i = t.get("tx_index").and_then(|b| b.as_u64()).unwrap_or(0);
                let hash = t
                    .get("transaction_hash")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if (h, i) > (cur_b, cur_i) && !hash.is_empty() {
                    fresh.push((h, i, hash, acct.clone()));
                }
            }
        }
        fresh.sort();
        fresh.dedup_by(|a, b| a.2 == b.2); // same tx seen via multiple accounts

        ticker.note_poll(fresh.len() as u64);
        if fresh.is_empty() {
            ticker.draw(&score); // idle polls still show liveness
            if score.txs >= cfg.max_txs {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(cfg.poll_secs));
            continue;
        }

        // ── fetch raw txs in hash-batches, replay first-external receipts ──
        let hashes: Vec<String> = {
            let mut hs: Vec<String> = fresh.iter().map(|(_, _, h, _)| h.clone()).collect();
            hs.dedup();
            hs
        };
        for chunk in hashes.chunks(20) {
            let raw = match post_json(
                &format!("{API}/v0/transactions"),
                &json!({ "tx_hashes": chunk }),
            ) {
                Ok(v) => v,
                Err(e) => {
                    score.fetch_err += 1;
                    println!("tx fetch error: {e}");
                    continue;
                }
            };
            let by_hash: HashMap<&str, &Value> = raw
                .get("transactions")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|t| {
                            t.get("transaction")
                                .and_then(|x| x.get("hash"))
                                .and_then(|h| h.as_str())
                                .map(|h| (h, t))
                        })
                        .collect()
                })
                .unwrap_or_default();

            for hash in chunk {
                let hash = hash.as_str();
                let Some(raw_tx) = by_hash.get(hash) else {
                    score.parse_surprise += 1;
                    save_fixture(
                        &cfg.fail_dir,
                        "missing-raw",
                        hash,
                        "na",
                        &json!({"hash": hash}),
                    );
                    continue;
                };
                if !processed.insert(hash.to_string()) {
                    continue;
                }
                score.txs += 1;

                let mut receipts: Vec<&Value> = raw_tx
                    .get("receipts")
                    .and_then(|r| r.as_array())
                    .map(|a| a.iter().collect())
                    .unwrap_or_default();
                receipts.sort_by_key(|r| {
                    (
                        r.get("receipt")
                            .and_then(|x| x.get("block_height"))
                            .and_then(|b| b.as_u64())
                            .unwrap_or(0),
                        r.get("receipt")
                            .and_then(|x| x.get("receipt_index"))
                            .and_then(|b| b.as_u64())
                            .unwrap_or(0),
                    )
                });

                // FIRST-EXTERNAL RULE: earliest receipt with receiver ∈ set and
                // predecessor ∉ set. Later set-receipts are DAG-covered (their
                // logic ran inside our replay via the promise machinery) or
                // second entries (rare; skipped, counted).
                let by_id: HashMap<&str, &Value> = receipts
                    .iter()
                    .filter_map(|r| {
                        r.pointer("/receipt/receipt_id")
                            .and_then(|x| x.as_str())
                            .map(|id| (id, *r))
                    })
                    .collect();
                let mut replayed = false;
                for r in &receipts {
                    let rid = r
                        .get("receipt")
                        .and_then(|x| x.get("receipt_id"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("na")
                        .to_string();
                    match extract(r, &deployed) {
                        Extract::Replay(mut target) if !replayed => {
                            replayed = true;
                            let (size, logs, failures) = fold_subtree(r, &by_id);
                            target.sub_size = size;
                            target.sub_logs = logs;
                            target.sub_failures = failures;
                            replay_one(
                                &chain,
                                &mut score,
                                &cfg,
                                hash,
                                &target,
                                &mut last_ts_secs,
                                &mut ticker,
                            );
                        }
                        Extract::Replay(_) => score.internal_covered += 1, // 2nd external entry in same tx
                        Extract::SkipInternal => score.internal_covered += 1,
                        Extract::SkipOther => score.other_receiver += 1,
                        Extract::SkipData => score.data_receipt += 1,
                        Extract::SkipTransfer => score.transfer_only += 1,
                        Extract::SkipMultiFc => score.multi_fc += 1,
                        Extract::ParseErr(e) => {
                            score.parse_surprise += 1;
                            save_fixture(
                                &cfg.fail_dir,
                                "parse",
                                hash,
                                &rid,
                                &json!({"err": e, "receipt": r}),
                            );
                        }
                    }
                    if replayed {
                        break; // one entry receipt per tx — rest is DAG-covered
                    }
                }
                ticker.draw(&score);
                if score.receipts % 200 == 0 && score.receipts > 0 && !cfg.errors_only {
                    ticker.finish(); // don't garble the \r line
                    score.print(&accounts);
                }
                if score.txs >= cfg.max_txs {
                    break 'outer;
                }
            }
        }
        // advance cursors past everything we processed this round
        for (h, i, hash, acct) in &fresh {
            if processed.contains(hash) {
                let e = cursors.entry(acct.clone()).or_insert((0, 0));
                if (*h, *i) > *e {
                    *e = (*h, *i);
                }
            }
        }
        save_cursors(&cfg.cursor_path, &cursors);
        if cfg.max_txs == u64::MAX {
            std::thread::sleep(std::time::Duration::from_secs(cfg.poll_secs));
        }
    }

    save_cursors(&cfg.cursor_path, &cursors);
    ticker.finish();
    if !cfg.errors_only {
        score.print(&accounts);
        println!("\nfixtures: {}   cursor: {}", cfg.fail_dir, cfg.cursor_path);
    }
    let infra: u64 = score.methods.values().map(|m| m.infra).sum();
    let upgrades: u64 = score.methods.values().map(|m| m.upgrade_suspect).sum();
    if infra > 0 || upgrades > 0 {
        println!(
            "🚨 {infra} INFRA + {upgrades} upgrade-suspect findings — fixtures in {}",
            cfg.fail_dir
        );
        std::process::exit(2);
    }
    println!("✅ done: {} receipts, no infra failures", score.receipts);
}

// ───────────────────────── replay one receipt ─────────────────────────

fn replay_one(
    chain: &MockChain,
    score: &mut Score,
    cfg: &Cfg,
    hash: &str,
    target: &ReplayTarget,
    last_ts_secs: &mut Option<i64>,
    ticker: &mut Ticker,
) {
    // Verbatim args: JSON contracts get their exact bytes (same as the old
    // UTF-8 string path), binary contracts (aurora's borsh EVM payloads) are
    // now replayable too — no more bad-args skip class.
    let arg_bytes = match base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &target.args_b64,
    ) {
        Ok(bytes) => bytes,
        Err(_) => {
            score.parse_surprise += 1;
            return;
        }
    };

    let ts_secs = (target.block_ts_nanos / 1_000_000_000) as i64;
    match last_ts_secs {
        None => chain.advance(ts_secs),
        Some(prev) if ts_secs > *prev => chain.advance(ts_secs - *prev),
        _ => {}
    }
    *last_ts_secs = Some(last_ts_secs.map_or(ts_secs, |p| p.max(ts_secs)));

    score.receipts += 1;
    ticker.note_receipt();
    let key = format!("{}::{}", target.receiver, target.method);
    let ms = score.methods.entry(key).or_default();
    ms.n += 1;

    let receiver_static: &str = Box::leak(target.receiver.clone().into_boxed_str());
    let result = chain
        .call(receiver_static, intern(&target.method))
        .args_bytes(arg_bytes)
        .from(target.predecessor.clone())
        .attach(target.deposit)
        .fire();

    // one-letter result class for the ticker/verbose feed
    let mut cls = String::new();
    let mut mock_gas: Option<u64> = None;
    match result {
        Err(e) => {
            ms.infra += 1;
            cls = "🚨infra".into();
            println!(
                "\n🚨 INFRA {}::{} @{}:{} tx {} — fire() Err: {e}",
                target.receiver, target.method, target.block_height, target.receipt_index, hash
            );
            save_fixture(
                &cfg.fail_dir,
                "infra",
                hash,
                &target.receipt_id,
                &json!({
                    "contract": target.receiver, "method": target.method, "predecessor": target.predecessor,
                    "args_b64": target.args_b64, "deposit": target.deposit.to_string(),
                    "mainnet": {"success": target.mn_success, "logs": target.mn_logs},
                    "mock_infra_error": e.to_string(),
                }),
            );
        }
        Ok(out) => {
            let mock_ok = out.ok;
            if out.ok {
                mock_gas = Some(out.gas_burned);
            }
            // upgrade detection: method vanished from the deployed build
            let upgrade = !mock_ok
                && out
                    .error
                    .as_deref()
                    .map(|e| e.contains("not found") || e.contains("MethodResolve"))
                    .unwrap_or(false);
            if upgrade {
                ms.upgrade_suspect += 1;
                cls = "⟳upgrade?".into();
                save_fixture(
                    &cfg.fail_dir,
                    "upgrade-suspect",
                    hash,
                    &target.receipt_id,
                    &json!({
                        "contract": target.receiver, "method": target.method,
                        "mock_error": out.error, "mainnet_failure": target.mn_failure,
                    }),
                );
                ticker.note_action(&target.receiver, &target.method, &cls, target.block_height);
                ticker.draw(score);
                return;
            }
            // ── subtree-fold comparison ──
            // Logs: mock logs = union over the whole executed promise DAG, so
            // compare against the fold of mainnet's receipt subtree (sub_size
            // == 1 ⇒ fold ≡ entry logs — identical to the old comparison).
            // Chain failures: mock receipt_failures (normalized promise-receipt
            // panics, execution order) vs mainnet subtree ExecutionErrors.
            if target.sub_size > 1 {
                ms.chain_n += 1;
            }
            let chain_exact =
                !target.sub_failures.is_empty() && out.receipt_failures == target.sub_failures;
            let chain_diverge = target.sub_size > 1
                && ((target.sub_failures.is_empty() && !out.receipt_failures.is_empty())
                    || (!target.sub_failures.is_empty() && !chain_exact));
            if chain_exact {
                ms.chain_fail_exact += 1;
            }
            let log_exact = mock_ok && target.mn_success && out.logs == target.sub_logs;
            if chain_diverge
                && mock_ok
                && target.mn_success
                && ms.chain_n - ms.chain_fail_exact <= 3
            {
                save_fixture(
                    &cfg.fail_dir,
                    "chain-class-diverge",
                    hash,
                    &target.receipt_id,
                    &json!({
                        "contract": target.receiver, "method": target.method,
                        "predecessor": target.predecessor,
                        "sub_size": target.sub_size,
                        "mainnet_subtree_failures": target.sub_failures,
                        "mock_receipt_failures": out.receipt_failures,
                        "mainnet_subtree_logs": target.sub_logs,
                        "mock_logs": out.logs,
                    }),
                );
            }
            match (mock_ok, target.mn_success) {
                (true, true) => {
                    ms.both_ok += 1;
                    // ── gas axis: mock instrumented burn vs mainnet receipt gas ──
                    if target.mn_gas > 0 {
                        ms.gas_n += 1;
                        let diff = out.gas_burned.abs_diff(target.mn_gas) as f64;
                        let within = diff / target.mn_gas as f64 <= cfg.gas_band;
                        if within {
                            ms.gas_ok += 1;
                        } else if log_exact && ms.gas_n - ms.gas_ok <= 2 {
                            // log-exact but gas off = pure execution-cost divergence
                            // (trie-walk calibration, host composites) — sample it
                            save_fixture(
                                &cfg.fail_dir,
                                "gas-diverge",
                                hash,
                                &target.receipt_id,
                                &json!({
                                    "contract": target.receiver, "method": target.method,
                                    "mock_gas": out.gas_burned, "mainnet_gas": target.mn_gas,
                                    "ratio": out.gas_burned as f64 / target.mn_gas as f64,
                                    "band": cfg.gas_band,
                                }),
                            );
                        }
                    }
                    if log_exact {
                        ms.log_exact += 1;
                        cls = "✓=exact".into();
                    } else {
                        cls = "✓ok".into();
                        if ms.both_ok - ms.log_exact <= 2 {
                            save_fixture(
                                &cfg.fail_dir,
                                "log-diverge",
                                hash,
                                &target.receipt_id,
                                &json!({
                                    "contract": target.receiver, "method": target.method,
                                    "predecessor": target.predecessor,
                                    "sub_size": target.sub_size,
                                    "mainnet_logs": target.sub_logs, "mock_logs": out.logs,
                                }),
                            );
                        }
                    }
                }
                (false, true) => {
                    ms.mock_fail_mainnet_ok += 1;
                    cls = "↓mock-fail".into();
                    if ms.mock_fail_mainnet_ok <= 3 {
                        save_fixture(
                            &cfg.fail_dir,
                            "mock-fail-mn-ok",
                            hash,
                            &target.receipt_id,
                            &json!({
                                "contract": target.receiver, "method": target.method,
                                "predecessor": target.predecessor, "args_b64": target.args_b64,
                                "deposit": target.deposit.to_string(),
                                "block_height": target.block_height,
                                "mock_error": out.error, "mock_panic": out.panic,
                                "mock_entry_trapped": out.entry_trapped, "mainnet_logs": target.mn_logs,
                            }),
                        );
                    }
                }
                (true, false) => {
                    ms.mock_ok_mainnet_fail += 1;
                    cls = "↑mock-only-ok".into();
                }
                (false, false) => {
                    ms.both_fail += 1;
                    let same_class =
                        out.panic.is_some() && out.panic.as_deref() == target.mn_failure.as_deref();
                    if same_class {
                        ms.same_class_fail += 1;
                        cls = "✗=class".into();
                    } else {
                        cls = "✗fail".into();
                    }
                    if ms.both_fail <= 2 || (!same_class && ms.both_fail - ms.same_class_fail <= 3)
                    {
                        save_fixture(
                            &cfg.fail_dir,
                            if same_class {
                                "both-fail"
                            } else {
                                "class-diverge"
                            },
                            hash,
                            &target.receipt_id,
                            &json!({
                                "contract": target.receiver, "method": target.method,
                                "predecessor": target.predecessor, "args_b64": target.args_b64,
                                "mock_error": out.error, "mock_panic": out.panic,
                                "mainnet_failure": target.mn_failure,
                                "sub_size": target.sub_size,
                                "mock_receipt_failures": out.receipt_failures,
                            }),
                        );
                    }
                }
            }
        }
    }

    // ── live UI: ticker + optional per-receipt feed ──
    ticker.note_action(&target.receiver, &target.method, &cls, target.block_height);
    if cfg.verbose {
        let gas_note = match (mock_gas, target.mn_gas) {
            (Some(mg), mn) if mn > 0 => format!(" gas:{:.2}x", mg as f64 / mn as f64),
            _ => String::new(),
        };
        println!(
            "→ {:<14}::{:<26} {:<12} sub:{}{} tx {}",
            target.receiver.chars().take(14).collect::<String>(),
            target.method,
            cls,
            target.sub_size,
            gas_note,
            hash
        );
    }
    ticker.draw(score);
}
