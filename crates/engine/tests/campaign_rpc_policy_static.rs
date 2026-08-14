//! WI-00 campaign RPC firewall + harness truth (fix-port campaign 2026-07-17).
//!
//! Proves the `HARNESS_PROFILE=mantis_local_offline` evidence boundary behaviorally: every
//! driver invocation here runs a COPY of `run-all-harnesses.sh` inside a throwaway workspace
//! with a scrubbed environment (`env_clear`), no `engine/.env`, and no `forge` on PATH, so a
//! regression can never leak a request to a paid provider from this test file itself.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NUMERIC_PINS: &[&str] = &[
    "460179231",
    "461623755",
    "464791929",
    "465686728",
    "465686729",
    "467233189",
];

const SELF_PINNED_SUITES: &[&str] = &[
    "MantisC2Predictor",
    "MantisCombosReplay",
    "MantisFamilyRealizedPnL",
    "MantisGasOracle",
];

const SUITE_COUNT: usize = 34;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("canonicalize repo root")
}

fn real_driver() -> PathBuf {
    repo_root().join("engine/deploy/aws/run-all-harnesses.sh")
}

fn real_contracts() -> PathBuf {
    repo_root().join("engine/contracts")
}

fn offline_pipeline() -> PathBuf {
    repo_root().join("anvil/scripts/offline_pipeline.sh")
}

fn dispatcher() -> PathBuf {
    repo_root().join("anvil/scripts/offline_manifest_dispatcher.sh")
}

fn url_guard_lib() -> PathBuf {
    repo_root().join("engine/deploy/aws/campaign_url_guard.sh")
}

fn netns_lib() -> PathBuf {
    repo_root().join("engine/deploy/aws/campaign_netns.sh")
}

fn fork_scan_lib() -> PathBuf {
    repo_root().join("engine/deploy/aws/campaign_fork_scan.sh")
}

fn rpc_counter() -> PathBuf {
    repo_root().join("anvil/scripts/rpc_counter.py")
}

/// The driver's companion shell libraries, copied alongside the driver in every fake workspace
/// so campaign profiles (which refuse to run without them) can source them. Conditional on
/// existence so RED runs — where a library does not exist yet — still exercise the old driver.
fn copy_driver_libs(root: &Path) {
    for lib in [url_guard_lib(), netns_lib(), fork_scan_lib()] {
        if lib.is_file() {
            fs::copy(
                &lib,
                root.join("engine/deploy/aws").join(lib.file_name().unwrap()),
            )
            .expect("copy driver lib into workspace");
        }
    }
}

/// Deterministic fake frozen state carrying exactly one account with non-empty code, so the
/// dispatcher can derive a per-state fingerprint (address+code) from the authenticated file and
/// prove the responding Anvil actually loaded it (WI-00c item 2).
fn fake_state_json(tag: &str) -> String {
    let mut acc: u32 = 0x9e_37_79_b9;
    for b in tag.bytes() {
        acc = acc.wrapping_mul(31).wrapping_add(b as u32);
    }
    let addr = format!("0x{:040x}", 0xa11ce_u64.wrapping_add(acc as u64));
    let code = format!("0x60{:06x}52", acc & 0x00ff_ffff);
    format!(
        "{{\"accounts\":{{\"{addr}\":{{\"nonce\":0,\"balance\":\"0x0\",\"code\":\"{code}\",\"storage\":{{}}}}}}}}"
    )
}

fn fresh_tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wi00-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create test tmp dir");
    dir
}

fn write_executable(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir for executable");
    fs::write(path, body).expect("write executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod executable");
}

/// Throwaway campaign workspace: driver copy, contracts symlinked from the real repo (the
/// closed-manifest validation greps real suite sources), and a complete fake frozen-state set.
struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(name: &str) -> Self {
        let root = fresh_tmp(name);
        fs::create_dir_all(root.join("engine/deploy/aws")).expect("mkdir engine/deploy/aws");
        fs::create_dir_all(root.join("anvil/state_offline/self")).expect("mkdir state_offline");
        fs::create_dir_all(root.join("out")).expect("mkdir out");
        fs::copy(
            real_driver(),
            root.join("engine/deploy/aws/run-all-harnesses.sh"),
        )
        .expect("copy driver into workspace");
        copy_driver_libs(&root);
        symlink(real_contracts(), root.join("engine/contracts"))
            .expect("symlink contracts into workspace");
        for pin in NUMERIC_PINS {
            fs::write(
                root.join(format!("anvil/state_offline/{pin}.json")),
                fake_state_json(pin),
            )
            .expect("write fake frozen state");
        }
        for suite in SELF_PINNED_SUITES {
            fs::write(
                root.join(format!("anvil/state_offline/self/{suite}.json")),
                fake_state_json(suite),
            )
            .expect("write fake self-pinned frozen state");
        }
        write_state_hash_manifest(&root);
        Self { root }
    }

    fn driver(&self) -> PathBuf {
        self.root.join("engine/deploy/aws/run-all-harnesses.sh")
    }

    fn out_dir(&self) -> PathBuf {
        self.root.join("out")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The dispatcher authenticates every frozen state against a sha256 manifest before loading it
/// (WI-00b gap 3.2). Fake workspaces carry a correct manifest by default; tamper tests overwrite
/// individual entries.
fn write_state_hash_manifest(root: &Path) {
    let status = Command::new("bash")
        .arg("-c")
        .arg(
            "cd \"$1\" && { ls *.json 2>/dev/null; ls self/*.json 2>/dev/null; } \
             | sort | xargs -r sha256sum > STATE_MANIFEST.sha256",
        )
        .arg("--")
        .arg(root.join("anvil/state_offline"))
        .status()
        .expect("spawn sha256 manifest writer");
    assert!(status.success(), "state hash manifest generation failed");
}

fn apply_campaign_env(cmd: &mut Command, home: &Path, out: &Path, extra_path: Option<&Path>) {
    cmd.env_clear();
    let path = match extra_path {
        Some(p) => format!("{}:/usr/bin:/bin", p.display()),
        None => "/usr/bin:/bin".to_owned(),
    };
    cmd.env("PATH", path);
    cmd.env("HOME", home);
    cmd.env("HARNESS_PROFILE", "mantis_local_offline");
    cmd.env("DRY_RUN", "true");
    cmd.env("SUBMIT_TX", "false");
    cmd.env("LIVE_SUBMIT", "false");
    cmd.env("SUBMIT_TRANSPORT", "counting-fake");
    cmd.env("ARBITRUM_RPC_URL", "http://127.0.0.1:8571");
    cmd.env("HARNESS_OUT", out);
}

/// Clean campaign-profile environment: every run starts from `env_clear`, so refusals below
/// are attributable to exactly one poisoned variable.
fn campaign_cmd(driver: &Path, home: &Path, out: &Path, extra_path: Option<&Path>) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg(driver);
    apply_campaign_env(&mut cmd, home, out, extra_path);
    cmd
}

/// Same as `campaign_cmd` but the driver runs inside a private, no-egress user+network
/// namespace (`unshare -rn`): billable egress is OS-impossible, which is exactly the
/// environment the non-enumerate offline profile requires (WI-00b P0.1).
fn campaign_cmd_in_netns(driver: &Path, home: &Path, out: &Path, extra_path: Option<&Path>) -> Command {
    let mut cmd = Command::new("unshare");
    cmd.args(["-rn", "--", "bash"]).arg(driver);
    apply_campaign_env(&mut cmd, home, out, extra_path);
    cmd
}

fn own_net_ns_inode() -> u64 {
    use std::os::unix::fs::MetadataExt;
    fs::metadata("/proc/self/ns/net")
        .expect("stat /proc/self/ns/net")
        .ino()
}

/// Mock anvil for dispatcher tests. Records its --load-state argument, then serves real JSON-RPC
/// on the INTERNAL anvil port (the dispatcher fronts it with a counting proxy on the public
/// port). It answers eth_chainId/eth_blockNumber and — crucially — eth_getCode from the very
/// state file it was told to load, so the dispatcher's per-state fingerprint probe is exercised
/// for real. `MOCK_ANVIL_MODE` selects the failure scenario.
const MOCK_ANVIL: &str = r#"#!/usr/bin/env bash
state=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--load-state" ]; then state="$a"; fi
  prev="$a"
done
printf 'ANVIL %s\n' "$state" >>"${DISPATCH_EVENT_LOG:?}"
if [ "${MOCK_ANVIL_MODE:-ok}" = die ]; then
  echo "mock anvil: simulated launch failure" >&2
  exit 1
fi
export MOCK_ANVIL_STATE="$state"
exec python3 - <<'PY'
import json, os
from http.server import BaseHTTPRequestHandler, HTTPServer
mode = os.environ.get("MOCK_ANVIL_MODE", "ok")
chain = "0x1" if mode == "wrongchain" else "0xa4b1"
block = "0x1bd95c00" if mode == "wrongblock" else "0x0"
codes = {}
try:
    with open(os.environ["MOCK_ANVIL_STATE"]) as fh:
        for addr, acc in (json.load(fh).get("accounts") or {}).items():
            c = acc.get("code", "0x")
            if isinstance(c, str) and c not in ("0x", ""):
                codes[addr.lower()] = c
except Exception:
    pass
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("content-length") or 0)
        try:
            req = json.loads(self.rfile.read(n) or b"{}")
        except Exception:
            req = {}
        m = req.get("method", "")
        if m == "eth_getCode":
            addr = (req.get("params") or ["0x"])[0]
            res = "0x" if mode == "wrongstate" else codes.get(str(addr).lower(), "0x")
        else:
            res = {"eth_chainId": chain, "eth_blockNumber": block}.get(m, "0x0")
        body = json.dumps({"jsonrpc": "2.0", "id": req.get("id", 1), "result": res}).encode()
        self.send_response(200)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a):
        pass
port = int(os.environ.get("DISPATCH_ANVIL_INTERNAL_PORT") or os.environ["DISPATCH_ANVIL_PORT"])
HTTPServer(("127.0.0.1", port), H).serve_forever()
PY
"#;

/// Mock forge for dispatcher tests. Campaign children run under an `env -i` allowlist, so the
/// only channels a confined child has are its isolated $HOME (child_home = group_N/child_home)
/// and that HOME's ancestry. It logs its invocation there and, so the primary counting proxy on
/// the real RPC path is exercised, makes one eth_chainId call to the validated ARBITRUM_RPC_URL.
const MOCK_FORGE: &str = r#"#!/usr/bin/env bash
printf 'FORGE %s\n' "$*" >>"${HOME:?}/forge_invocations.log"
if [ -n "${ARBITRUM_RPC_URL:-}" ]; then
  curl -sS --max-time 2 -H 'content-type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}' \
    "$ARBITRUM_RPC_URL" >/dev/null 2>&1 || true
fi
printf '%s\n' 'Suite result: ok. 1 passed; 0 failed; 0 skipped; finished in 1.00ms (1.00ms CPU time)'
"#;

/// Dispatcher-test workspace: fake states + hash manifest + dispatcher/sink copies + mock bins.
struct DispatcherHarness {
    ws: Workspace,
    mock_bin: PathBuf,
    event_log: PathBuf,
}

impl DispatcherHarness {
    fn new(name: &str) -> Self {
        let ws = Workspace::new(name);
        fs::create_dir_all(ws.root.join("anvil/scripts")).expect("mkdir anvil/scripts");
        fs::copy(
            dispatcher(),
            ws.root.join("anvil/scripts/offline_manifest_dispatcher.sh"),
        )
        .expect("copy dispatcher into workspace");
        if rpc_counter().is_file() {
            fs::copy(rpc_counter(), ws.root.join("anvil/scripts/rpc_counter.py"))
                .expect("copy rpc counter into workspace");
        }
        let mock_bin = ws.root.join("mockbin");
        write_executable(&mock_bin.join("anvil"), MOCK_ANVIL);
        write_executable(&mock_bin.join("forge"), MOCK_FORGE);
        let event_log = ws.root.join("events.log");
        Self {
            ws,
            mock_bin,
            event_log,
        }
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::new("bash");
        cmd.arg(self.ws.root.join("anvil/scripts/offline_manifest_dispatcher.sh"));
        cmd.env_clear();
        cmd.env(
            "PATH",
            format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.mock_bin.display()),
        );
        cmd.env("HOME", &self.ws.root);
        cmd.env("DISPATCH_EVENT_LOG", &self.event_log);
        cmd.env("DISPATCH_ANVIL_PORT", "8571");
        cmd.env("HARNESS_OUT_ROOT", self.ws.out_dir());
        cmd
    }

    fn events(&self) -> String {
        fs::read_to_string(&self.event_log).unwrap_or_default()
    }

    /// Every confined forge invocation logs into its group's isolated child HOME.
    fn forge_invocations(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let out = self.ws.out_dir();
        let Ok(entries) = fs::read_dir(&out) else {
            return lines;
        };
        for entry in entries.flatten() {
            let log = entry.path().join("child_home/forge_invocations.log");
            if let Ok(content) = fs::read_to_string(&log) {
                for l in content.lines() {
                    lines.push(l.to_owned());
                }
            }
        }
        lines
    }
}

fn run(cmd: &mut Command) -> (Output, String) {
    let output = cmd.output().expect("spawn bash driver");
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output, combined)
}

fn assert_refused(ws: &Workspace, poison: &[(&str, &str)], expect_marker: &str) {
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    for (key, value) in poison {
        cmd.env(key, value);
    }
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success(),
        "driver must refuse poisoned env {poison:?}, got success:\n{text}"
    );
    assert!(
        text.contains("CAMPAIGN-FIREWALL REFUSED"),
        "poison {poison:?} must be stopped by the campaign firewall (marker missing):\n{text}"
    );
    assert!(
        text.contains(expect_marker),
        "refusal for {poison:?} must name its cause ({expect_marker:?}):\n{text}"
    );
    assert!(
        !text.contains("── forge Mantis"),
        "refusal for {poison:?} must stop the driver before any suite starts:\n{text}"
    );
    assert!(
        !text.contains("ENUM SUITE="),
        "refusal for {poison:?} must stop the driver before enumeration:\n{text}"
    );
}

fn assert_flag_refused(ws: &Workspace, unset: &[&str], set: &[(&str, &str)], marker: &str) {
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    for key in unset {
        cmd.env_remove(key);
    }
    for (key, value) in set {
        cmd.env(key, value);
    }
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success(),
        "driver must refuse submit-flag drift unset={unset:?} set={set:?}:\n{text}"
    );
    assert!(
        text.contains("CAMPAIGN-FIREWALL REFUSED") && text.contains(marker),
        "submit-flag drift unset={unset:?} set={set:?} must be refused naming {marker:?}:\n{text}"
    );
}

#[test]
fn campaign_profile_rejects_quicknode_alchemy_aws_and_non_loopback_submit_endpoints() {
    let ws = Workspace::new("firewall-endpoints");

    // Paid-provider hostname anywhere in the environment (value scan).
    assert_refused(
        &ws,
        &[(
            "ARBITRUM_RPC_URL_HOT",
            "https://arb-mainnet.g.alchemy.com/v2/deadbeef",
        )],
        "paid-provider",
    );
    assert_refused(
        &ws,
        &[(
            "ENGINE_WS_URL",
            "wss://methodical-x.arbitrum-mainnet.quiknode.pro/deadbeef/",
        )],
        "paid-provider",
    );
    assert_refused(
        &ws,
        &[("METRICS_SINK", "https://s3.us-east-2.amazonaws.com/bucket")],
        "paid-provider",
    );

    // Provider secret / AWS environment by NAME, regardless of value.
    assert_refused(&ws, &[("ALCHEMY_API_KEY", "fake")], "forbidden provider");
    assert_refused(
        &ws,
        &[("QUICKNODE_TOKEN", "fake")],
        "forbidden provider",
    );
    assert_refused(
        &ws,
        &[("AWS_SECRET_ACCESS_KEY", "fake")],
        "forbidden provider",
    );
    assert_refused(&ws, &[("AWS_PROFILE", "mantis")], "forbidden provider");

    // Non-loopback Forge RPC endpoint.
    assert_refused(
        &ws,
        &[("ARBITRUM_RPC_URL", "https://198.51.100.7:8545")],
        "non-loopback",
    );

    // Non-counting submit transport.
    assert_refused(&ws, &[("SUBMIT_TRANSPORT", "relay")], "SUBMIT_TRANSPORT");

    // Positive control: a clean loopback campaign environment passes the firewall (it may still
    // be blocked later by missing frozen state, but never by a firewall refusal).
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    let (_, text) = run(&mut cmd);
    assert!(
        text.contains("CAMPAIGN-FIREWALL OK"),
        "clean campaign env must pass the firewall preflight:\n{text}"
    );
    assert!(
        !text.contains("CAMPAIGN-FIREWALL REFUSED"),
        "clean campaign env must not trip any refusal:\n{text}"
    );
}

#[test]
fn campaign_profile_requires_dry_run_and_all_submit_flags_false() {
    let ws = Workspace::new("firewall-flags");

    assert_flag_refused(&ws, &["DRY_RUN"], &[], "DRY_RUN");
    assert_flag_refused(&ws, &[], &[("DRY_RUN", "false")], "DRY_RUN");
    assert_flag_refused(&ws, &[], &[("SUBMIT_TX", "true")], "SUBMIT_TX");
    assert_flag_refused(&ws, &[], &[("LIVE_SUBMIT", "true")], "LIVE_SUBMIT");
    assert_flag_refused(&ws, &["SUBMIT_TX"], &[], "SUBMIT_TX");
    assert_flag_refused(&ws, &["LIVE_SUBMIT"], &[], "LIVE_SUBMIT");

    // Positive control: the exact asserted trio passes the firewall.
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    let (_, text) = run(&mut cmd);
    assert!(
        text.contains("CAMPAIGN-FIREWALL OK"),
        "DRY_RUN=true SUBMIT_TX=false LIVE_SUBMIT=false must pass the firewall:\n{text}"
    );
}

#[test]
fn forge_driver_offline_mode_rejects_any_cache_miss_or_external_rpc() {
    let ws = Workspace::new("offline-cache");

    // Positive path: with a complete frozen-state set, the offline enumeration dry-run lists
    // every manifest suite with its exact parent pin and resolved frozen state, and exits 0.
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    let (output, text) = run(&mut cmd);
    assert!(
        output.status.success(),
        "offline enumeration with complete frozen state must succeed:\n{text}"
    );
    let mut pins_seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut enum_lines = 0usize;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("ENUM SUITE=") else {
            continue;
        };
        enum_lines += 1;
        let mut fields = rest.split_whitespace();
        let suite = fields.next().expect("ENUM suite field");
        let pin = fields
            .next()
            .and_then(|f| f.strip_prefix("PIN="))
            .expect("ENUM pin field");
        let state = fields
            .next()
            .and_then(|f| f.strip_prefix("STATE="))
            .expect("ENUM state field");
        assert!(
            suite.ends_with(".Fork.t.sol"),
            "enumerated suite name malformed: {line}"
        );
        assert_ne!(state, "MISSING", "complete state set must resolve: {line}");
        if pin == "self" {
            let name = suite.trim_end_matches(".Fork.t.sol");
            assert!(
                state.ends_with(&format!("state_offline/self/{name}.json")),
                "self-pinned suite must resolve its own frozen state: {line}"
            );
        } else {
            assert!(
                pin.chars().all(|c| c.is_ascii_digit()),
                "pin must be numeric or self: {line}"
            );
            assert!(
                state.ends_with(&format!("state_offline/{pin}.json")),
                "numeric pin must resolve its frozen state file: {line}"
            );
        }
        *pins_seen.entry(pin.to_owned()).or_default() += 1;
    }
    assert_eq!(
        enum_lines, SUITE_COUNT,
        "enumeration must list every manifest suite exactly once:\n{text}"
    );
    assert_eq!(
        pins_seen.get("self").copied().unwrap_or(0),
        SELF_PINNED_SUITES.len(),
        "enumeration lost a self-pinned suite:\n{text}"
    );
    for pin in NUMERIC_PINS {
        assert!(
            pins_seen.contains_key(*pin),
            "enumeration lost parent pin {pin}:\n{text}"
        );
    }

    // Cache miss: deleting one frozen state must fail the offline gate, naming the pin as a
    // blocking prerequisite (recorded, not fetched).
    fs::remove_file(ws.root.join("anvil/state_offline/461623755.json"))
        .expect("remove frozen state to simulate cache miss");
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success(),
        "offline mode must reject a frozen-state cache miss:\n{text}"
    );
    assert!(
        text.contains("BLOCKING-PREREQUISITE") && text.contains("461623755"),
        "cache miss must be recorded as a blocking prerequisite naming the pin:\n{text}"
    );

    // External RPC: offline mode refuses a non-loopback endpoint before anything runs.
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    cmd.env("ARBITRUM_RPC_URL", "https://arb1.example.com");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success() && text.contains("non-loopback"),
        "offline mode must refuse an external Forge RPC endpoint:\n{text}"
    );
}

#[test]
fn campaign_profile_requires_forge_scope() {
    // WI-00c item 2 + residual 4.1: a campaign profile must REFUSE any scope other than
    // `forge`. `HARNESS_SCOPE=default` under a campaign profile would run unconfined
    // Cargo/replay children on the host network, so it must be refused before it starts —
    // in particular the `HARNESS_ENUMERATE=1 + HARNESS_SCOPE=default` composition that used to
    // waive the egress guard and then execute the full default harness.
    let ws = Workspace::new("requires-forge-scope");

    for (label, scope, enumerate) in [
        ("default+enumerate", "default", Some("1")),
        ("default", "default", None),
        ("unset-scope", "", None),
    ] {
        let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
        if !scope.is_empty() {
            cmd.env("HARNESS_SCOPE", scope);
        } else {
            cmd.env_remove("HARNESS_SCOPE");
        }
        if let Some(e) = enumerate {
            cmd.env("HARNESS_ENUMERATE", e);
        }
        let (output, text) = run(&mut cmd);
        assert!(
            !output.status.success(),
            "campaign profile [{label}] must refuse a non-forge scope:\n{text}"
        );
        assert!(
            text.contains("HARNESS_SCOPE=forge"),
            "refusal [{label}] must require HARNESS_SCOPE=forge:\n{text}"
        );
        assert!(
            !text.contains("ALL OFFLINE HARNESSES PASSED")
                && !text.contains("ALL HARNESSES PASSED"),
            "campaign profile [{label}] must never reach a default-harness banner:\n{text}"
        );
    }
}

#[test]
fn enumerate_flag_is_bound_to_forge_scope() {
    // WI-00c item 2: `HARNESS_ENUMERATE=1` may waive the egress isolation requirement only
    // because it "runs nothing" — that is ONLY true under `HARNESS_SCOPE=forge`. It must never
    // suppress isolation for a scope that actually executes children.
    let ws = Workspace::new("enumerate-bound");
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_ENUMERATE", "1");
    cmd.env("HARNESS_SCOPE", "default");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success() && text.contains("HARNESS_SCOPE=forge"),
        "HARNESS_ENUMERATE=1 with a non-forge scope must be refused:\n{text}"
    );
}

#[test]
fn forge_fmt_is_absent_from_every_campaign_command() {
    // The complete WI-00 campaign command inventory must exist, contain no `forge fmt`
    // anywhere, and route all Foundry work through the closed driver (no bare `forge test`
    // outside `run-all-harnesses.sh`).
    let inventory = [
        repo_root().join("engine/deploy/aws/run-all-harnesses.sh"),
        repo_root().join("engine/deploy/aws/tests/run-all-harnesses-target-dir-mocked.sh"),
        offline_pipeline(),
        dispatcher(),
        url_guard_lib(),
        netns_lib(),
        fork_scan_lib(),
        rpc_counter(),
    ];
    for path in &inventory {
        let content = fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "campaign command inventory incomplete: {} unreadable ({e})",
                path.display()
            )
        });
        assert!(
            !content.contains("forge fmt"),
            "{} must never invoke forge fmt (proven destructive)",
            path.display()
        );
    }
    for path in [offline_pipeline(), dispatcher()] {
        let content = fs::read_to_string(&path).expect("read campaign script");
        assert!(
            !content.contains("forge test"),
            "{} must route all Foundry work through run-all-harnesses.sh, never bare forge test",
            path.display()
        );
        assert!(
            content.contains("run-all-harnesses.sh"),
            "{} must invoke the closed harness driver",
            path.display()
        );
    }
}

#[test]
fn offline_pipeline_replaces_alchemy_warm_with_seed_lava_and_verify_offline() {
    let script = fs::read_to_string(offline_pipeline()).expect("read offline_pipeline.sh");

    // Static truth FIRST (also keeps this test from ever executing the legacy Alchemy warm
    // path): the pipeline must no longer extract a paid RPC URL from engine/.env, and it must
    // expose the explicit campaign subcommands.
    assert!(
        !script.contains("grep '^ARBITRUM_RPC_URL='"),
        "offline_pipeline.sh must not read a paid provider URL out of engine/.env"
    );
    for subcommand in ["seed-lava", "verify-offline"] {
        assert!(
            script.contains(subcommand),
            "offline_pipeline.sh must expose the {subcommand} subcommand"
        );
    }

    // Behavioral: the retired warm subcommand fails closed, pointing at seed-lava.
    let (output, text) = run(Command::new("bash")
        .arg(offline_pipeline())
        .arg("warm")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", fresh_tmp("pipeline-warm-home")));
    assert!(
        !output.status.success() && text.contains("seed-lava"),
        "warm must be retired in favor of the explicit seed-lava subcommand:\n{text}"
    );

    // Behavioral: seed-lava refuses a paid-provider endpoint (Lava is the ONLY legal remote).
    let (output, text) = run(Command::new("bash")
        .arg(offline_pipeline())
        .arg("seed-lava")
        .arg("461623755")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", fresh_tmp("pipeline-seed-home"))
        .env("LAVA_RPC_URL", "https://arb-mainnet.g.alchemy.com/v2/x"));
    assert!(
        !output.status.success() && text.contains("SEED-LAVA REFUSED"),
        "seed-lava must refuse a non-Lava endpoint:\n{text}"
    );

    // Behavioral: seed-lava with no endpoint at all is refused, never defaulted.
    let (output, text) = run(Command::new("bash")
        .arg(offline_pipeline())
        .arg("seed-lava")
        .arg("461623755")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", fresh_tmp("pipeline-seed-unset-home")));
    assert!(
        !output.status.success() && text.contains("SEED-LAVA REFUSED"),
        "seed-lava must refuse when LAVA_RPC_URL is unset:\n{text}"
    );
}

#[test]
fn offline_manifest_dispatcher_loads_frozen_state_per_suite_before_driver() {
    assert!(
        dispatcher().is_file(),
        "anvil/scripts/offline_manifest_dispatcher.sh does not exist"
    );
    let h = DispatcherHarness::new("dispatcher");
    let (output, text) = run(&mut h.cmd());

    let events = h.events();
    let anvil_starts: Vec<&str> = events
        .lines()
        .filter(|l| l.starts_with("ANVIL "))
        .collect();

    // One anvil launch per pin group: 6 numeric pins + 4 self-pinned suites. The anvil loads a
    // TOCTOU-safe private copy (state_private_N.json), so the authenticated ORIGINAL each group
    // used is proven via the dispatcher's own `DISPATCH group=... state=...` lines.
    assert_eq!(
        anvil_starts.len(),
        NUMERIC_PINS.len() + SELF_PINNED_SUITES.len(),
        "dispatcher must restart anvil once per frozen-state group:\n{events}\n{text}"
    );
    let group_lines: Vec<&str> = text
        .lines()
        .filter(|l| l.trim_start().starts_with("DISPATCH group="))
        .collect();
    for pin in NUMERIC_PINS {
        assert!(
            group_lines
                .iter()
                .any(|l| l.contains(&format!("state_offline/{pin}.json"))),
            "dispatcher never authenticated+loaded frozen state for pin {pin}:\n{}",
            group_lines.join("\n")
        );
    }
    for suite in SELF_PINNED_SUITES {
        assert!(
            group_lines
                .iter()
                .any(|l| l.contains(&format!("state_offline/self/{suite}.json"))),
            "dispatcher never authenticated+loaded frozen state for self-pinned {suite}:\n{}",
            group_lines.join("\n")
        );
    }

    // WI-00c item 8: the dispatcher must have moved itself into a private no-egress network
    // namespace whose transition it VERIFIED (ns inode differs from this test process's, and it
    // is loopback-only) — never inferred from route sampling.
    let own_ns = own_net_ns_inode().to_string();
    let ns_line = text
        .lines()
        .find(|l| l.contains("NETNS_ISOLATION"))
        .unwrap_or_else(|| panic!("dispatcher must report its verified netns isolation:\n{text}"));
    assert!(
        ns_line.contains("transition=verified")
            && ns_line.contains("interfaces=loopback-only")
            && ns_line.contains("ns_net_inode=")
            && !ns_line.contains(&format!("ns_net_inode={own_ns}")),
        "dispatcher must VERIFY the namespace transition + loopback-only interfaces (test ns inode {own_ns}): {ns_line}\n{text}"
    );

    // WI-00c item 6: the real Forge RPC path (primary ARBITRUM_RPC_URL -> supervised counting
    // proxy -> verified Anvil) is measured. Mock forge makes one eth_chainId call per suite, so
    // the primary connection counter must be nonzero, and the paid-alias sink must record zero.
    let rpc_line = text
        .lines()
        .find(|l| l.contains("RPC_PATH_CONNECTIONS"))
        .unwrap_or_else(|| panic!("dispatcher must report measured RPC path connections:\n{text}"));
    assert!(
        rpc_line.contains("paid_alias_attempts=0"),
        "no suite may reach a paid-alias endpoint:\n{rpc_line}"
    );
    assert!(
        !rpc_line.contains("primary=0"),
        "the primary counting proxy must have observed the real-path RPC traffic:\n{rpc_line}"
    );

    // Every manifest suite ran exactly once, through the driver's confined forge invocation
    // (the invocation log lives in each group's isolated child HOME): serial (-j 1), alias
    // fork URL, and NO numeric fork-block pin (the loaded frozen state embodies the pin; the
    // genesis is at block 0 by design).
    let forge_runs = h.forge_invocations();
    assert_eq!(
        forge_runs.len(),
        SUITE_COUNT,
        "dispatcher must run every manifest suite exactly once (confined child logs):\n{forge_runs:?}\n{text}"
    );
    for line in &forge_runs {
        assert!(
            line.contains("-j 1"),
            "offline forge invocation lost its serial shape: {line}"
        );
        let self_pinned = SELF_PINNED_SUITES
            .iter()
            .any(|s| line.contains(&format!("test/{s}.Fork.t.sol")));
        if self_pinned {
            // Self-pinned suites createSelectFork inside their own source off the exported
            // loopback endpoint; the driver passes them no fork arguments.
            assert!(
                !line.contains("--fork-url"),
                "self-pinned suite must not receive driver fork arguments: {line}"
            );
        } else {
            assert!(
                line.contains("--fork-url arbitrum"),
                "offline forge invocation lost its alias fork shape: {line}"
            );
        }
        assert!(
            !line.contains("--fork-block-number"),
            "offline forge invocation must not pin a block that the frozen genesis cannot serve: {line}"
        );
    }

    // Grouping: each announced state group's suites ran inside exactly that group's driver
    // invocation (per-group child HOME), so no suite can run before its state is loaded.
    let mut groups_checked = 0usize;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("DISPATCH group=") else {
            continue;
        };
        let idx = rest.split_whitespace().next().expect("group index");
        let suites = rest
            .split(" suites=")
            .nth(1)
            .expect("group suite list")
            .trim();
        let group_log = h
            .ws
            .out_dir()
            .join(format!("group_{idx}/child_home/forge_invocations.log"));
        let content = fs::read_to_string(&group_log).unwrap_or_else(|e| {
            panic!("group {idx} confined forge log missing ({e}): {}", group_log.display())
        });
        for suite in suites.split(',') {
            assert!(
                content.contains(&format!("test/{suite}")),
                "suite {suite} did not run inside its state group {idx}:\n{content}"
            );
        }
        groups_checked += 1;
    }
    assert_eq!(
        groups_checked,
        NUMERIC_PINS.len() + SELF_PINNED_SUITES.len(),
        "dispatcher must announce every state group:\n{text}"
    );

    // Fail-closed aggregate: the mocked one-test-per-suite totals (34) cannot satisfy the
    // closed manifest expectation (161), so the dispatcher must refuse to go green.
    assert!(
        !output.status.success(),
        "dispatcher must fail closed on aggregate test-count drift:\n{text}"
    );
    assert!(
        text.contains("aggregate"),
        "dispatcher must name the aggregate drift when refusing:\n{text}"
    );
}

// ─── WI-00b hardening regressions (codex/WI00_VERIFY_2026-07-17.md) ─────────────────────────
// Every test below encodes one CONCRETE bypass/misconfiguration from the independent review and
// proves the firewall now refuses it.

/// Gap 1.1: raw URL text was checked instead of the parsed authority. The review's concrete
/// bypass `http://127.0.0.1:9@arb-mainnet.g.%61lchemy.com/v2/<key>` starts with accepted
/// loopback TEXT but parses to an Alchemy DESTINATION (userinfo + percent-encoded host).
#[test]
fn offline_url_guard_refuses_userinfo_percent_and_non_loopback_hosts() {
    let ws = Workspace::new("url-guard-offline");

    // codex's exact offline bypass: loopback-looking userinfo, percent-encoded provider host.
    assert_refused(
        &ws,
        &[(
            "ARBITRUM_RPC_URL",
            "http://127.0.0.1:9@arb-mainnet.g.%61lchemy.com/v2/deadbeef",
        )],
        "non-loopback",
    );
    // Userinfo is refused even when the real host IS loopback (ambiguous authority).
    assert_refused(
        &ws,
        &[("ARBITRUM_RPC_URL", "http://deadbeef@127.0.0.1:8571")],
        "non-loopback",
    );
    // Percent-encoding anywhere in the authority is ambiguous → refused.
    assert_refused(
        &ws,
        &[("ARBITRUM_RPC_URL", "http://127.0.0.%31:8571")],
        "non-loopback",
    );
    // `localhost` is DNS-resolvable (hosts-file remappable) — offline requires the exact
    // loopback literal, so it is refused too.
    assert_refused(
        &ws,
        &[("ARBITRUM_RPC_URL", "http://localhost:8571")],
        "non-loopback",
    );
    // Loopback-prefixed lookalike hostname.
    assert_refused(
        &ws,
        &[("ARBITRUM_RPC_URL", "http://127.0.0.1.example.com:8571")],
        "non-loopback",
    );
    // WI-00c parser nit: a malformed bracket authority must be rejected outright, not coerced
    // to `[::1]` by lenient extraction.
    assert_refused(
        &ws,
        &[("ARBITRUM_RPC_URL", "http://[::1]garbage]:8571")],
        "non-loopback",
    );
    assert_refused(
        &ws,
        &[("ARBITRUM_RPC_URL", "http://[::1:8571")],
        "non-loopback",
    );

    // Positive controls: the two exact loopback literals still pass the firewall.
    for ok in ["http://127.0.0.1:8571", "http://[::1]:8571"] {
        let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
        cmd.env("HARNESS_SCOPE", "forge");
        cmd.env("HARNESS_ENUMERATE", "1");
        cmd.env("ARBITRUM_RPC_URL", ok);
        let (_, text) = run(&mut cmd);
        assert!(
            text.contains("CAMPAIGN-FIREWALL OK"),
            "exact loopback literal {ok} must pass the firewall:\n{text}"
        );
    }
}

/// Gap 3.4 (driver layer): the Lava allow rule accepted `lava.build` ANYWHERE in the raw value.
/// The parsed authority must be an exact label-boundary Lava hostname.
#[test]
fn lava_url_guard_refuses_userinfo_lookalike_and_plain_http() {
    let ws = Workspace::new("url-guard-lava");
    let refuse_cases = [
        // codex's exact seed bypass: Lava text as userinfo, percent-encoded Alchemy host.
        "https://lava.build@arb-mainnet.g.%61lchemy.com/v2/deadbeef",
        // Longer attacker hostname containing the Lava name on a non-label boundary.
        "https://lava.build.evil.example/rpc",
        "https://xlava.build/rpc",
        // Plain http to a Lava host is not the controlled seeding transport.
        "http://lava.build/rpc",
        // An IP literal can be provider-backed; never accept it as "Lava".
        "https://198.51.100.7/rpc",
    ];
    for bad in refuse_cases {
        let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
        cmd.env("HARNESS_SCOPE", "forge");
        cmd.env("HARNESS_ENUMERATE", "1");
        cmd.env("HARNESS_PROFILE", "mantis_lava_seed");
        cmd.env("ARBITRUM_RPC_URL", bad);
        let (output, text) = run(&mut cmd);
        assert!(
            !output.status.success()
                && text.contains("CAMPAIGN-FIREWALL REFUSED")
                && text.to_lowercase().contains("lava"),
            "mantis_lava_seed must refuse non-Lava parsed authority {bad}:\n{text}"
        );
    }

    // Positive control: an exact-label-boundary Lava hostname passes the firewall.
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    cmd.env("HARNESS_PROFILE", "mantis_lava_seed");
    cmd.env("ARBITRUM_RPC_URL", "https://arb1.lava.build/lava-referer/x");
    let (_, text) = run(&mut cmd);
    assert!(
        text.contains("CAMPAIGN-FIREWALL OK"),
        "an exact Lava hostname must pass the mantis_lava_seed firewall:\n{text}"
    );
}

/// Gap 3.4 (pipeline layer): `seed-lava` itself must parse the destination, not scan raw text.
/// Runs a COPY of the pipeline in a throwaway workspace, inside a no-egress namespace, with no
/// forge on PATH — so even a regression cannot reach any remote.
#[test]
fn seed_lava_refuses_parsed_non_lava_destinations() {
    let ws = Workspace::new("seed-lava-url");
    fs::create_dir_all(ws.root.join("anvil/scripts")).expect("mkdir anvil/scripts");
    fs::copy(
        offline_pipeline(),
        ws.root.join("anvil/scripts/offline_pipeline.sh"),
    )
    .expect("copy pipeline into workspace");

    let run_seed = |lava_url: &str| -> (Output, String) {
        let mut cmd = Command::new("unshare");
        cmd.args(["-rn", "--", "bash"]);
        cmd.arg(ws.root.join("anvil/scripts/offline_pipeline.sh"));
        cmd.arg("seed-lava");
        cmd.arg("461623755");
        cmd.env_clear();
        cmd.env("PATH", "/usr/bin:/bin");
        cmd.env("HOME", &ws.root);
        cmd.env("LAVA_RPC_URL", lava_url);
        run(&mut cmd)
    };

    for bad in [
        "https://lava.build@arb-mainnet.g.%61lchemy.com/v2/deadbeef",
        "https://lava.build.evil.example/rpc",
        "https://xlava.build/rpc",
        "http://lava.build/rpc",
    ] {
        let (output, text) = run_seed(bad);
        assert!(
            !output.status.success() && text.contains("SEED-LAVA REFUSED"),
            "seed-lava must refuse non-Lava parsed destination {bad}:\n{text}"
        );
    }

    // Positive control for the URL gate only: an exact Lava hostname must NOT be refused by the
    // hostname rule (the run later fails on the missing forge binary, which is fine — the seed
    // step never reaches any remote inside this no-egress namespace).
    let (_, text) = run_seed("https://arb1.lava.build/lava-referer/x");
    assert!(
        !text.contains("SEED-LAVA REFUSED"),
        "an exact Lava hostname must pass the seed-lava URL gate:\n{text}"
    );
}

/// Gap 2.1: the env scan consumed `compgen -e` through a process substitution whose failure
/// looked like an empty, successful scan. An exported function named `compgen` must not be able
/// to blind the provider-name/value checks.
#[test]
fn firewall_env_scan_fails_closed_when_compgen_is_shadowed() {
    let ws = Workspace::new("compgen-shadow");
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    // Exported bash function shadowing compgen (imported by the driver's bash).
    cmd.env("BASH_FUNC_compgen%%", "() { return 1; }");
    // The poison the blinded scan would miss.
    cmd.env("ALCHEMY_API_KEY", "fake");
    let (output, text) = run(&mut cmd);
    // WI-00d strengthens this: ANY imported Bash function (including a shadowed compgen) is now
    // refused up front by the un-shadowable detection, before the env scan even runs.
    assert!(
        !output.status.success()
            && (text.contains("CAMPAIGN-FIREWALL REFUSED")
                || text.contains("inherited Bash function")),
        "a shadowed compgen must not blind the env scan (ALCHEMY_API_KEY was set):\n{text}"
    );
}

/// Gap 2.2: `grep` status 2 (scanner failure) took the same clean branch as status 1 (no
/// match). A failing scanner must refuse, never pass.
#[test]
fn forbidden_primitive_scan_failure_fails_closed() {
    let ws = Workspace::new("grep-guard-failure");
    let stub_bin = ws.root.join("stubbin");
    // grep stub: fails ONLY for the fork-test safety-scan pattern; passes everything else
    // through to the real grep (so manifest/count checks behave normally).
    write_executable(
        &stub_bin.join("grep"),
        r#"#!/usr/bin/env bash
case "$*" in
  *'vm\.(broadcast'*) exit 2 ;;
esac
exec /usr/bin/grep "$@"
"#,
    );
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), Some(&stub_bin));
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success(),
        "safety-scan scanner failure must fail the gate, not read as clean:\n{text}"
    );
    assert!(
        text.contains("safety scan") || text.contains("scan failed"),
        "the refusal must name the failed safety scan:\n{text}"
    );
}

/// Gap 2.3: firewall-artifact writes were unchecked; making the artifact path unwritable (a
/// directory) silently continued. Every firewall write must be checked.
#[test]
fn firewall_artifact_write_failure_fails_closed() {
    let ws = Workspace::new("artifact-write");
    fs::create_dir_all(ws.out_dir().join("campaign_firewall.txt"))
        .expect("pre-create artifact path as a directory");
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success(),
        "an unwritable firewall artifact must refuse the run:\n{text}"
    );
    assert!(
        text.to_lowercase().contains("firewall artifact"),
        "the refusal must name the firewall artifact:\n{text}"
    );
}

/// Gap 2.4: a nonempty filter with no valid token selected zero suites and could exit green;
/// HARNESS_ENUMERATE was treated as enabled for ANY nonempty value (including `0`).
#[test]
fn malformed_suite_filter_or_enumerate_flag_fails_closed() {
    let ws = Workspace::new("filter-guard");

    let run_with = |filter: Option<&str>, enumerate: &str| -> (Output, String) {
        let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
        cmd.env("HARNESS_SCOPE", "forge");
        cmd.env("HARNESS_ENUMERATE", enumerate);
        if let Some(f) = filter {
            cmd.env("HARNESS_FORGE_SUITE_FILTER", f);
        }
        run(&mut cmd)
    };

    // codex's concrete misconfiguration: HARNESS_FORGE_SUITE_FILTER=','.
    let (output, text) = run_with(Some(","), "1");
    assert!(
        !output.status.success() && text.contains("filter"),
        "a bare-comma suite filter must refuse, never select zero suites:\n{text}"
    );
    // Empty token inside an otherwise valid list.
    let (output, text) = run_with(
        Some("MantisRoutes.Fork.t.sol,,MantisRoutesAll.Fork.t.sol"),
        "1",
    );
    assert!(
        !output.status.success() && text.contains("filter"),
        "an empty filter token must refuse:\n{text}"
    );
    // Duplicate token.
    let (output, text) = run_with(
        Some("MantisRoutes.Fork.t.sol,MantisRoutes.Fork.t.sol"),
        "1",
    );
    assert!(
        !output.status.success() && text.contains("filter"),
        "a duplicate filter token must refuse:\n{text}"
    );
    // HARNESS_ENUMERATE accepts only unset or exactly '1'.
    for bad in ["0", "true", "yes"] {
        let (output, text) = run_with(None, bad);
        assert!(
            !output.status.success() && text.contains("HARNESS_ENUMERATE"),
            "HARNESS_ENUMERATE={bad} must refuse (only unset or '1' are legal):\n{text}"
        );
    }
}

/// Gap 2.5 + structural wiring: fail-closed contracts that must exist verbatim in the driver
/// and its companion scripts.
#[test]
fn driver_fail_closed_static_contracts() {
    let driver_text = fs::read_to_string(real_driver()).expect("read driver");
    assert!(
        driver_text.contains("run_phase1_forge_scope || FAILED=1"),
        "gap 2.5: the Forge-scope helper needs a top-level `|| FAILED=1` backstop"
    );
    assert!(
        driver_text.contains("builtin compgen -e"),
        "gap 2.1: the env scan must enumerate through `builtin compgen -e` into checked data"
    );
    assert!(
        driver_text.contains("campaign_url_guard.sh"),
        "gap 1.1: the driver must load the shared parsed-URL guard library"
    );
    assert!(
        driver_text.contains("Seeding-blocker taxonomy"),
        "gap 4c: the driver manifest must record the corrected seeding-blocker taxonomy"
    );
    // WI-00c item 3: the trusted URL parse output must be emitted with `builtin printf` so an
    // exported `printf` function cannot forge it.
    let url_guard_text = fs::read_to_string(url_guard_lib()).expect("read url guard");
    assert!(
        url_guard_text.contains("builtin printf") && !url_guard_text.contains("\n  printf "),
        "item 1: campaign_url_guard.sh must serialize its trusted tuple with `builtin printf`"
    );
    // WI-00d P0-A: imported functions are refused via an UN-SHADOWABLE detection (reading the
    // kernel env image with `$(< /proc/self/environ)` + a `case *BASH_FUNC_*`), not a
    // `read`-based scrub loop (which is itself shadowable). The dispatcher and the driver's
    // campaign preflight must carry it; the legacy scripts too.
    let dispatcher_text_early = fs::read_to_string(dispatcher()).expect("read dispatcher");
    for (label, text) in [
        ("dispatcher", &dispatcher_text_early),
        ("driver", &driver_text),
    ] {
        assert!(
            text.contains("/proc/self/environ") && text.contains("BASH_FUNC_"),
            "{label} must detect imported functions via the kernel env image (un-shadowable)"
        );
    }
    // WI-00c item 2: campaign profiles require HARNESS_SCOPE=forge.
    assert!(
        driver_text.contains("HARNESS_SCOPE=forge"),
        "item 2: campaign profiles must require HARNESS_SCOPE=forge"
    );
    // WI-00c item 8: the driver's offline egress control is loopback-only netns verification,
    // NOT route sampling. The retired sampling sentinel must be gone.
    assert!(
        !driver_text.contains("203.0.113.1") && !driver_text.contains("no-route-verified"),
        "item 8: the driver must not infer isolation from sampled routes"
    );
    assert!(
        driver_text.contains("campaign_netns.sh"),
        "item 8: the driver must load the shared netns-isolation library"
    );
    // WI-00c residual 4.3: campaign children run offline (no dependency/compiler fetch).
    assert!(
        driver_text.contains("FOUNDRY_OFFLINE=true"),
        "residual 4.3: campaign Forge children must run with FOUNDRY_OFFLINE=true"
    );
    // WI-00c item 8 (Lava exception): the Forge source safety scan must detect a second fork
    // endpoint (createFork/createSelectFork/literal URL) via the shared scan library.
    assert!(
        driver_text.contains("campaign_fork_scan.sh"),
        "item 8: the driver must load the shared Forge fork-source scan library"
    );
    let fork_scan_text = fs::read_to_string(fork_scan_lib()).expect("read fork scan lib");
    assert!(
        fork_scan_text.contains("createSelectFork") && fork_scan_text.contains("createFork"),
        "the fork-source scan must detect createFork/createSelectFork endpoints"
    );

    let pipeline_text = fs::read_to_string(offline_pipeline()).expect("read pipeline");
    assert!(
        pipeline_text.contains("campaign_url_guard.sh"),
        "gap 3.4: seed-lava must use the shared parsed-URL guard library"
    );
    // WI-00c item 8: mantis_lava_seed is an authorized NON-zero-egress step, explicitly excluded
    // from the structural zero-spend guarantee — the driver must record that, not just a
    // "controlled exception" note.
    assert!(
        driver_text.contains("EXCLUDED") && driver_text.to_lowercase().contains("lava"),
        "item 8: the driver must document mantis_lava_seed as excluded from the zero-spend guarantee"
    );
    let dispatcher_text = fs::read_to_string(dispatcher()).expect("read dispatcher");
    assert!(
        dispatcher_text.contains("WI00_DISPATCH_IN_NETNS")
            && !dispatcher_text.contains("203.0.113.1"),
        "item 8: the dispatcher must enter its namespace UNCONDITIONALLY (no route sampling)"
    );
    // WI-00d P0-B: the re-exec uses an ABSOLUTE trusted unshare (no PATH resolution). The netns
    // library must expose the inode fail-closed (no `unknown` acceptance).
    assert!(
        dispatcher_text.contains("/usr/bin/unshare") || dispatcher_text.contains("/bin/unshare"),
        "P0-B: the dispatcher must re-exec via an ABSOLUTE trusted unshare"
    );
    let netns_text = fs::read_to_string(netns_lib()).expect("read netns lib");
    assert!(
        !netns_text.contains("printf 'unknown") && !netns_text.contains("printf \"unknown"),
        "P0-A/fail-close: campaign_netns_inode must not EMIT an accepted `unknown` inode sentinel"
    );
    // WI-00d P0-A: the loopback-only proof must not depend on a shadowable `read`.
    assert!(
        !netns_text.contains("< /proc/net/dev") || !netns_text.contains("read -r"),
        "P0-A: the loopback-only check must not parse /proc/net/dev via a shadowable `read`"
    );
    // WI-00d direct-driver binding: the driver's non-enumerate offline path requires
    // dispatcher ownership (sole entrypoint).
    assert!(
        driver_text.contains("WI00_DISPATCH_OWNED"),
        "direct-driver binding: offline non-enumerate work must require the dispatcher ownership marker"
    );
    assert!(
        dispatcher_text.contains("WI00_DISPATCH_OWNED"),
        "the dispatcher must issue its ownership marker to the driver"
    );
    assert!(
        url_guard_lib().is_file() && netns_lib().is_file() && fork_scan_lib().is_file(),
        "the shared driver libraries must exist next to the driver"
    );
    assert!(
        rpc_counter().is_file(),
        "item 6: the supervised counting RPC proxy/sink must exist for real-path measurement"
    );
}

/// Gap 1.3: legacy network/live entrypoints ignored the campaign profile and read engine/.env
/// directly. Each must refuse a campaign profile BEFORE its first .env read.
#[test]
fn legacy_entrypoints_refuse_campaign_profiles() {
    let scripts = [
        "anvil/scripts/replay_all_arbs.sh",
        "anvil/scripts/dump_states.sh",
        "anvil/scripts/investigate_errors.sh",
        "anvil/scripts/gate2b_offline.sh",
        "engine/scripts/run_full_opts.sh",
    ];

    // Static gate FIRST: the refusal guard must appear BEFORE the first `.env` reference. This
    // assertion failing also prevents the behavioral half from ever executing an unguarded
    // legacy script (which could touch real artifacts).
    for rel in scripts {
        let path = repo_root().join(rel);
        let content =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel} failed: {e}"));
        let guard_at = content.find("mantis_local_offline").unwrap_or_else(|| {
            panic!("{rel} has no campaign-profile refusal guard (gap 1.3)")
        });
        let env_at = content
            .find(".env")
            .expect("legacy script no longer references .env — update this test");
        assert!(
            guard_at < env_at,
            "{rel}: the campaign-profile guard must run BEFORE the first .env read \
             (guard at byte {guard_at}, .env at byte {env_at})"
        );
    }

    // Behavioral: with a campaign profile set the script must exit nonzero, naming the refusal,
    // before doing anything. Runs inside a no-egress namespace with a bare PATH (no
    // cast/forge/anvil/cargo) and neutralized inputs, so even a regression cannot spend.
    for (rel, profile) in [
        ("anvil/scripts/replay_all_arbs.sh", "mantis_local_offline"),
        ("anvil/scripts/dump_states.sh", "mantis_local_offline"),
        ("anvil/scripts/investigate_errors.sh", "mantis_lava_seed"),
        ("anvil/scripts/gate2b_offline.sh", "mantis_local_offline"),
        ("engine/scripts/run_full_opts.sh", "mantis_local_offline"),
    ] {
        let home = fresh_tmp("legacy-guard-home");
        let neutral_in = home.join("empty_input.csv");
        fs::write(&neutral_in, "").expect("write neutral input");
        let mut cmd = Command::new("unshare");
        cmd.args(["-rn", "--", "bash"]);
        cmd.arg(repo_root().join(rel));
        cmd.env_clear();
        cmd.env("PATH", "/usr/bin:/bin");
        cmd.env("HOME", &home);
        cmd.env("HARNESS_PROFILE", profile);
        // Neutralize replay_all_arbs inputs in case of regression: empty input exits early.
        cmd.env("IN", &neutral_in);
        // WI-00d P0 §2.6: an imported `exit` function must NOT be able to shadow the refusal into
        // a fall-through that then reads engine/.env and issues a paid RPC. The guard must stop
        // regardless (un-shadowable exit).
        cmd.env("BASH_FUNC_exit%%", "() { return 0; }");
        let (output, text) = run(&mut cmd);
        assert!(
            !output.status.success(),
            "{rel} must refuse HARNESS_PROFILE={profile} even with an imported exit function:\n{text}"
        );
        assert!(
            text.contains("not a campaign entrypoint")
                || text.contains("REFUSED")
                || text.contains("inherited Bash function"),
            "{rel} must NAME the campaign-profile refusal:\n{text}"
        );
        // The refusal must stop before the paid RPC path: no cast/anvil/forge/cargo was on PATH,
        // and .env is unreadable here, but a fall-through would still print the script's own work
        // markers. Assert none of the scripts' post-guard activity ran.
        assert!(
            !text.contains("blocks to process")
                && !text.contains("investigating")
                && !text.contains("PHASE 1"),
            "{rel} must not reach any post-guard work under a campaign profile:\n{text}"
        );
        let _ = fs::remove_dir_all(&home);
    }
}

/// Gap 1.2: something already listening on the dispatch port could impersonate the spawned
/// Anvil. Defence in depth even though the fresh namespace already starts with nothing bound:
/// the dispatcher must refuse a pre-open port. The scenario pre-binds the port inside an
/// already-isolated namespace and marks the namespace as dispatcher-issued
/// (`WI00_DISPATCH_IN_NETNS=1`) so the pre-open check runs against the port the test pre-bound.
#[test]
fn dispatcher_refuses_pre_opened_dispatch_port() {
    let h = DispatcherHarness::new("dispatcher-preopen");
    let scenario = h.ws.root.join("preopen_scenario.sh");
    write_executable(
        &scenario,
        r#"#!/usr/bin/env bash
set -u
ip link set lo up 2>/dev/null || /usr/sbin/ip link set lo up 2>/dev/null || true
python3 - <<'PY' &
import os, socket, time
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(os.environ["DISPATCH_ANVIL_PORT"])))
s.listen(16)
time.sleep(600)
PY
listener=$!
trap 'kill "$listener" 2>/dev/null' EXIT
sleep 0.5
bash "$1"
"#,
    );
    let mut cmd = Command::new("unshare");
    cmd.args(["-rn", "--", "bash"]);
    cmd.arg(&scenario);
    cmd.arg(h.ws.root.join("anvil/scripts/offline_manifest_dispatcher.sh"));
    cmd.env_clear();
    cmd.env(
        "PATH",
        format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", h.mock_bin.display()),
    );
    cmd.env("HOME", &h.ws.root);
    cmd.env("DISPATCH_EVENT_LOG", &h.event_log);
    cmd.env("DISPATCH_ANVIL_PORT", "8571");
    cmd.env("HARNESS_OUT_ROOT", h.ws.out_dir());
    // Already isolated by the scenario's `unshare -rn`; skip the dispatcher's own re-exec so the
    // pre-open check runs in the namespace where the port is bound. The dispatcher still
    // independently verifies loopback-only isolation AND a proven transition, so we hand it the
    // real host (parent) ns inode; the scenario's unshare ns differs from it.
    cmd.env("WI00_DISPATCH_IN_NETNS", "1");
    cmd.env("WI00_PARENT_NETNS_INODE", own_net_ns_inode().to_string());
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success(),
        "a pre-opened dispatch port must refuse the run:\n{text}"
    );
    assert!(
        text.contains("pre-open") || text.contains("already"),
        "the refusal must name the pre-owned port:\n{text}"
    );
    // The impostor listener must never have been treated as a ready anvil: no group may have
    // produced a confined forge invocation.
    assert!(
        h.forge_invocations().is_empty(),
        "no suite may run against a port the dispatcher did not open:\n{text}"
    );
}

/// Gap 1.2 + WI-00c item 2: readiness must track the SPAWNED process, parse JSON `.result`
/// EXACTLY (not by substring), and bind a per-state fingerprint (eth_getCode of an account from
/// the authenticated fixture) so a generic empty responder cannot impersonate the loaded state.
#[test]
fn dispatcher_refuses_dead_anvil_and_impostor_identity() {
    // (a) anvil dies right after launch → refusal must name the death, fast.
    let h = DispatcherHarness::new("dispatcher-dead");
    let mut cmd = h.cmd();
    cmd.env("MOCK_ANVIL_MODE", "die");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success() && text.contains("died"),
        "a dead anvil must be named as such (not a generic port timeout):\n{text}"
    );

    // (b) something answers but with the wrong chain id → identity refusal.
    let h = DispatcherHarness::new("dispatcher-wrongchain");
    let mut cmd = h.cmd();
    cmd.env("MOCK_ANVIL_MODE", "wrongchain");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success() && text.contains("identity"),
        "a wrong chain id must fail the identity probe:\n{text}"
    );

    // (c) right chain id but a live-head block number → it is not the frozen block-0 genesis.
    let h = DispatcherHarness::new("dispatcher-wrongblock");
    let mut cmd = h.cmd();
    cmd.env("MOCK_ANVIL_MODE", "wrongblock");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success() && text.contains("fingerprint"),
        "a non-genesis block number must fail the state fingerprint probe:\n{text}"
    );

    // (d) chain id + block 0x0 correct, but eth_getCode returns 0x for the fixture account →
    // the responder did NOT load the authenticated state; the per-state fingerprint must catch it.
    let h = DispatcherHarness::new("dispatcher-wrongstate");
    let mut cmd = h.cmd();
    cmd.env("MOCK_ANVIL_MODE", "wrongstate");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success() && text.contains("fingerprint"),
        "a responder that did not load the authenticated state must fail the fingerprint:\n{text}"
    );
    assert!(
        h.forge_invocations().is_empty(),
        "no suite may run against an unverified Anvil:\n{text}"
    );
}

/// WI-00c item 2: a caller-supplied `ANVIL_BIN` wrapper is the review's concrete impostor
/// vector; campaign dispatch must reject the override outright.
#[test]
fn dispatcher_rejects_anvil_bin_override() {
    let h = DispatcherHarness::new("dispatcher-anvilbin");
    write_executable(
        &h.ws.root.join("evil_anvil.sh"),
        "#!/usr/bin/env bash\nexec sleep 600\n",
    );
    let mut cmd = h.cmd();
    cmd.env("ANVIL_BIN", h.ws.root.join("evil_anvil.sh"));
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success() && text.contains("ANVIL_BIN"),
        "a campaign ANVIL_BIN override must be refused:\n{text}"
    );
    assert!(
        h.forge_invocations().is_empty(),
        "no suite may run under a caller-chosen anvil binary:\n{text}"
    );
}

/// Gap 3.2: nonempty state files were accepted as frozen snapshots on `-s` alone. The
/// dispatcher must verify each group's state file against the sha256 manifest first.
#[test]
fn dispatcher_verifies_state_hash_before_each_group() {
    // Tampered state: correct manifest, then one state file changes after manifest creation.
    let h = DispatcherHarness::new("dispatcher-hash");
    fs::write(
        h.ws.root.join("anvil/state_offline/465686728.json"),
        "{\"accounts\":{\"0x00\":{}}}",
    )
    .expect("tamper one frozen state");
    let (output, text) = run(&mut h.cmd());
    assert!(
        !output.status.success(),
        "a state file that fails hash verification must refuse:\n{text}"
    );
    assert!(
        text.contains("state-auth") && text.contains("465686728"),
        "the refusal must name state authentication and the offending state:\n{text}"
    );

    // Missing manifest entirely: fail closed, never trust -s.
    let h = DispatcherHarness::new("dispatcher-nomanifest");
    fs::remove_file(h.ws.root.join("anvil/state_offline/STATE_MANIFEST.sha256"))
        .expect("remove state manifest");
    let (output, text) = run(&mut h.cmd());
    assert!(
        !output.status.success() && text.contains("state-auth"),
        "a missing state-auth manifest must refuse:\n{text}"
    );
}

/// WI-00c item 6: the paid-alias sink must be a real, supervised counter. A suite that reaches a
/// paid alias must produce a nonzero measured count and a refusal.
#[test]
fn dispatcher_counter_refuses_measured_paid_alias_attempt() {
    let h = DispatcherHarness::new("dispatcher-sink");
    // The dispatcher points every secondary/paid alias at the fatal sink and passes the sink
    // URL to the child via a marker so this test can drive a paid-alias attempt deterministically
    // (POKE_SINK_URL is read by the confinement-aware mock forge below).
    let mock_bin = h.ws.root.join("mockbin");
    write_executable(
        &mock_bin.join("forge"),
        r#"#!/usr/bin/env bash
printf 'FORGE %s\n' "$*" >>"${HOME:?}/forge_invocations.log"
url="$(cat "${HOME:?}/../../POKE_SINK_URL" 2>/dev/null || true)"
if [ -n "$url" ]; then
  curl -sS --max-time 2 --data '{"jsonrpc":"2.0","id":1,"method":"eth_call","params":[]}' "$url" >/dev/null 2>&1 || true
fi
printf '%s\n' 'Suite result: ok. 1 passed; 0 failed; 0 skipped; finished in 1.00ms (1.00ms CPU time)'
"#,
    );
    // The sink listens on DISPATCH_SINK_PORT; default 9. Record it for the mock to reach.
    fs::write(h.ws.out_dir().join("POKE_SINK_URL"), "http://127.0.0.1:9")
        .expect("write sink poke url");
    let (output, text) = run(&mut h.cmd());
    assert!(
        !output.status.success(),
        "a measured paid-alias attempt must fail the dispatch:\n{text}"
    );
    assert!(
        text.contains("paid_alias_attempts=") && !text.contains("paid_alias_attempts=0"),
        "the measured paid-alias count must be nonzero when the sink WAS hit:\n{text}"
    );
}

/// WI-00c item 6: the counting proxy/sink must be supervised throughout every group — if it dies
/// after readiness, the dispatch fails closed rather than deriving a meaningless zero.
#[test]
fn dispatcher_fails_closed_when_counter_dies_mid_run() {
    let h = DispatcherHarness::new("dispatcher-counter-die");
    let mut cmd = h.cmd();
    // Self-test hook honored by rpc_counter.py: exit shortly after binding (simulates a counter
    // that dies after readiness), which the per-group kill -0 supervision must catch.
    cmd.env("RPC_COUNTER_SELFTEST_DIE", "1");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success(),
        "a counter that dies after readiness must fail the dispatch:\n{text}"
    );
    assert!(
        text.to_lowercase().contains("counter") || text.to_lowercase().contains("proxy"),
        "the refusal must name the failed counter/proxy supervision:\n{text}"
    );
}

/// WI-00d P0-B: a caller-`PATH` `unshare` wrapper must be DEFEATED — the dispatcher re-execs via
/// an ABSOLUTE trusted `unshare`, so a PATH wrapper never runs (cannot spend then delegate).
#[test]
fn path_unshare_wrapper_is_defeated() {
    let h = DispatcherHarness::new("dispatcher-pathwrap");
    let spent = h.ws.root.join("SPENT_MARKER");
    // A malicious `unshare` earlier on PATH would "spend" (touch SPENT_MARKER) then exec the real
    // unshare. Because the dispatcher uses an absolute trusted unshare, this never runs.
    write_executable(
        &h.mock_bin.join("unshare"),
        &format!(
            "#!/usr/bin/env bash\ntouch '{}'\nexec /usr/bin/unshare \"$@\"\n",
            spent.display()
        ),
    );
    let (_output, text) = run(&mut h.cmd());
    assert!(
        !spent.exists(),
        "a PATH `unshare` wrapper must never run (absolute unshare required):\n{text}"
    );
    // And the dispatcher must still have genuinely isolated (real absolute unshare used).
    assert!(
        text.contains("NETNS_ISOLATION transition=verified"),
        "the dispatcher must still isolate via the absolute unshare:\n{text}"
    );
}

/// WI-00d fail-close: a caller-preset recursion sentinel WITHOUT a proven transition (i.e. run
/// directly in the host namespace, which is not loopback-only) must REFUSE — never accept an
/// `unknown`/`assumed-caller-isolated` transition.
#[test]
fn dispatcher_refuses_preset_sentinel_without_isolation() {
    let h = DispatcherHarness::new("dispatcher-presetsentinel");
    let mut cmd = h.cmd();
    // Skip the dispatcher's own re-exec, but do NOT provide a real isolated namespace: this runs
    // in the test's host namespace, which has real interfaces.
    cmd.env("WI00_DISPATCH_IN_NETNS", "1");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success(),
        "a preset recursion sentinel in a non-isolated namespace must refuse:\n{text}"
    );
    assert!(
        text.to_lowercase().contains("loopback") || text.to_lowercase().contains("netns") || text.to_lowercase().contains("transition"),
        "the refusal must name the missing isolation/transition:\n{text}"
    );
    assert!(
        !h.events().contains("ANVIL "),
        "no state group may start without proven isolation:\n{text}"
    );
}

/// WI-00d P0-C: caller-controlled port text must be validated as purely numeric BEFORE any Bash
/// arithmetic, so a crafted `DISPATCH_ANVIL_PORT` cannot execute a command via arithmetic
/// evaluation before namespace entry.
#[test]
fn crafted_dispatch_port_cannot_execute_before_netns() {
    let h = DispatcherHarness::new("dispatcher-craftedport");
    let pwned = h.ws.root.join("PWNED");
    let mut cmd = h.cmd();
    // Array-subscript arithmetic recursively evaluates variable contents; a command substitution
    // in the value would run if the port reached `$(( ))` unvalidated.
    cmd.env(
        "DISPATCH_ANVIL_PORT",
        format!("x[$(touch '{}')]", pwned.display()),
    );
    let (output, text) = run(&mut cmd);
    assert!(
        !pwned.exists(),
        "a crafted DISPATCH_ANVIL_PORT must never execute a command (numeric validation first):\n{text}"
    );
    assert!(
        !output.status.success() && text.to_lowercase().contains("numeric"),
        "a non-numeric DISPATCH_ANVIL_PORT must be refused:\n{text}"
    );
}

/// WI-00d P0-A: the imported-function scrub must itself be un-shadowable. An exported `read`
/// function (which could forge the loopback-only proof or blind the scrub) must make a campaign
/// entrypoint REFUSE, not proceed.
#[test]
fn exported_read_cannot_forge_the_isolation_proof() {
    // (a) dispatcher: any imported Bash function refuses before namespace work.
    let h = DispatcherHarness::new("dispatcher-shadowread");
    let mut cmd = h.cmd();
    cmd.env("BASH_FUNC_read%%", "() { return 1; }");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success() && text.contains("inherited Bash function"),
        "an imported read must make the dispatcher refuse (un-shadowable detection):\n{text}"
    );
    assert!(
        !h.events().contains("ANVIL "),
        "no state group may start with an imported function present:\n{text}"
    );

    // (b) driver campaign preflight: same un-shadowable refusal (enumerate path, no netns needed).
    let ws = Workspace::new("driver-shadowread");
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    cmd.env("BASH_FUNC_read%%", "() { return 1; }");
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success() && text.contains("inherited Bash function"),
        "an imported read must make the driver campaign preflight refuse:\n{text}"
    );
}

/// WI-00d direct-driver binding: the dispatcher is the SOLE campaign entrypoint for non-enumerate
/// offline work. A direct driver invocation (even inside a loopback-only namespace) without the
/// dispatcher-issued ownership marker must REFUSE.
#[test]
fn offline_driver_requires_dispatcher_ownership() {
    let ws = Workspace::new("driver-notowned");
    let mock_bin = ws.root.join("mockbin");
    write_executable(
        &mock_bin.join("forge"),
        "#!/usr/bin/env bash\nprintf '%s\\n' 'Suite result: ok. 1 passed; 0 failed; 0 skipped;'\n",
    );
    // Inside a real loopback-only namespace (so the isolation check itself would pass), but with
    // NO dispatcher ownership marker: must still refuse.
    let mut cmd = campaign_cmd_in_netns(&ws.driver(), &ws.root, &ws.out_dir(), Some(&mock_bin));
    cmd.env("HARNESS_SCOPE", "forge"); // non-enumerate offline
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success(),
        "direct non-enumerate offline driver use must be refused (dispatcher is the sole entrypoint):\n{text}"
    );
    assert!(
        text.to_lowercase().contains("dispatcher"),
        "the refusal must point at the dispatcher as the sole entrypoint:\n{text}"
    );
    assert!(
        !text.contains("── forge Mantis"),
        "no suite may run via a direct, non-dispatcher driver invocation:\n{text}"
    );
}

/// Gaps 1.4 + 3.3: campaign children inherited the full parent environment, the real HOME and
/// its ambient Foundry caches. Children must run under an env -i allowlist with an isolated
/// empty HOME and DOTENV_PATH=/dev/null.
#[test]
fn campaign_children_get_confined_env_home_and_dotenv() {
    let ws = Workspace::new("child-confinement");
    let mock_bin = ws.root.join("mockbin");
    // Mock forge dumps its entire environment into its (isolated) HOME.
    write_executable(
        &mock_bin.join("forge"),
        r#"#!/usr/bin/env bash
{ echo "FORGE_ENV_BEGIN $*"; env; echo "FORGE_ENV_END"; } >>"${HOME:?}/forge_env_dump.txt"
printf '%s\n' 'Suite result: ok. 1 passed; 0 failed; 0 skipped; finished in 1.00ms (1.00ms CPU time)'
"#,
    );
    // Non-enumerate offline forge scope must run inside the no-egress namespace.
    let mut cmd = campaign_cmd_in_netns(&ws.driver(), &ws.root, &ws.out_dir(), Some(&mock_bin));
    cmd.env("HARNESS_SCOPE", "forge");
    // Simulates the dispatcher-invoked driver (non-enumerate offline is dispatcher-only, WI-00d).
    cmd.env("WI00_DISPATCH_OWNED", "1");
    // Canary that must NOT leak into any campaign child.
    cmd.env("OPERATOR_SESSION_TOKEN", "canary-must-not-leak");
    // WI-00c residual 4.2: a caller-chosen HARNESS_CHILD_HOME must be IGNORED (it could point
    // the "isolated" HOME back at the operator's ambient .foundry cache).
    cmd.env("HARNESS_CHILD_HOME", &ws.root);
    let (_, text) = run(&mut cmd);

    let dump_path = ws.out_dir().join("child_home/forge_env_dump.txt");
    let dump = fs::read_to_string(&dump_path).unwrap_or_else(|e| {
        panic!(
            "confined child env dump missing at {} ({e}) — children are not confined to a fresh \
             isolated HOME under HARNESS_OUT:\n{text}",
            dump_path.display()
        )
    });
    assert!(
        !dump.contains("OPERATOR_SESSION_TOKEN"),
        "campaign children must not inherit unrelated operator environment:\n{dump}"
    );
    assert!(
        dump.contains("DOTENV_PATH=/dev/null"),
        "campaign children must get DOTENV_PATH=/dev/null:\n{dump}"
    );
    // WI-00c residual 4.3: children must run offline so no compilation/dependency fetch is
    // attempted (decisive in the egress-enabled Lava phase).
    assert!(
        dump.contains("FOUNDRY_OFFLINE=true"),
        "campaign children must run with FOUNDRY_OFFLINE=true:\n{dump}"
    );
    assert!(
        dump.contains("ARBITRUM_RPC_URL=http://127.0.0.1:8571"),
        "campaign children must keep the validated loopback endpoint:\n{dump}"
    );
    let home_line = dump
        .lines()
        .find(|l| l.starts_with("HOME="))
        .expect("child env dump has no HOME line");
    assert!(
        home_line.contains("child_home") && !home_line.trim_end().ends_with(&ws.root.display().to_string()),
        "campaign children must run under a fresh isolated HOME (never the caller override), got: {home_line}"
    );
    // The isolated HOME must not expose the ambient Foundry RPC/fork cache.
    assert!(
        !ws.out_dir().join("child_home/.foundry").exists(),
        "the isolated child HOME must start without an ambient .foundry cache"
    );
}

/// WI-00c item 1 + item 8: the URL guard's trusted parse output and the isolation/egress
/// decision must be immune to shell-function shadowing. An exported `printf` that forges the
/// Lava parse, and an exported `python3` that fakes "no route", must both be neutralized.
#[test]
fn exported_functions_cannot_forge_url_parse_or_skip_isolation() {
    // (a) exported printf forging the Lava tuple for a real customer CNAME → must be refused.
    let ws = Workspace::new("shadow-printf");
    fs::create_dir_all(ws.root.join("anvil/scripts")).expect("mkdir anvil/scripts");
    fs::copy(
        offline_pipeline(),
        ws.root.join("anvil/scripts/offline_pipeline.sh"),
    )
    .expect("copy pipeline");
    let mut cmd = Command::new("unshare");
    cmd.args(["-rn", "--", "bash"]);
    cmd.arg(ws.root.join("anvil/scripts/offline_pipeline.sh"));
    cmd.arg("seed-lava");
    cmd.arg("461623755");
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin");
    cmd.env("HOME", &ws.root);
    cmd.env("LAVA_RPC_URL", "https://rpc.customer-cname.example/v2/key");
    // Exported bash function shadowing printf, forging `https|lava.build|` for the parser query.
    cmd.env(
        "BASH_FUNC_printf%%",
        "() { if [ \"${1-}\" = '%s|%s|%s\\n' ]; then builtin printf 'https|lava.build|\\n'; else builtin printf \"$@\"; fi; }",
    );
    let (output, text) = run(&mut cmd);
    // WI-00d closes this even harder: the pipeline's un-shadowable detection refuses ANY imported
    // function up front (the printf forgery never even runs).
    assert!(
        !output.status.success()
            && (text.contains("SEED-LAVA REFUSED") || text.contains("inherited Bash function")),
        "an exported printf must not forge the Lava parse for a customer CNAME:\n{text}"
    );

    // (b) exported python3 forcing "no route" must not let the offline driver skip isolation.
    // Under WI-00d the driver refuses ANY imported function outright (an even stronger outcome
    // than falling through to the isolation check).
    let ws2 = Workspace::new("shadow-python");
    let mut cmd = campaign_cmd(&ws2.driver(), &ws2.root, &ws2.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge"); // non-enumerate → isolation required
    cmd.env(
        "BASH_FUNC_python3%%",
        "() { return 1; }",
    );
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success(),
        "an exported python3 returning nonzero must not let offline work run unisolated:\n{text}"
    );
    assert!(
        text.to_lowercase().contains("isolat")
            || text.to_lowercase().contains("netns")
            || text.contains("inherited Bash function"),
        "the refusal must name the missing namespace isolation or the imported function:\n{text}"
    );
}

/// WI-00c item 8: non-enumerate offline driver work run OUTSIDE an isolated namespace must be
/// refused — there is no route-sampling escape. (A normal host with a default route previously
/// exercised route sampling; this proves the driver now requires proven loopback-only isolation.)
#[test]
fn offline_driver_refuses_when_not_in_isolated_netns() {
    let ws = Workspace::new("driver-not-isolated");
    let mock_bin = ws.root.join("mockbin");
    write_executable(
        &mock_bin.join("forge"),
        "#!/usr/bin/env bash\nprintf '%s\\n' 'Suite result: ok. 1 passed; 0 failed; 0 skipped;'\n",
    );
    // NOT wrapped in unshare -rn: the test process's namespace still has real interfaces.
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), Some(&mock_bin));
    cmd.env("HARNESS_SCOPE", "forge"); // non-enumerate offline
    let (output, text) = run(&mut cmd);
    assert!(
        !output.status.success(),
        "non-enumerate offline driver work must refuse outside an isolated namespace:\n{text}"
    );
    assert!(
        text.to_lowercase().contains("isolat") || text.to_lowercase().contains("netns")
            || text.to_lowercase().contains("loopback"),
        "the refusal must name the isolation requirement (point at the dispatcher):\n{text}"
    );
    assert!(
        !text.contains("── forge Mantis"),
        "no suite may start outside an isolated namespace:\n{text}"
    );
}

/// Gap 3.1 honesty + deferred 4a doc note: the firewall artifact must not print constant zero
/// counters it never measured, and must state that no-submit currently rests on the engine
/// DRY_RUN/SUBMIT_TX gates (counting-fake transport binding is deferred).
#[test]
fn firewall_artifact_is_honest_about_counters_and_submit_authority() {
    let ws = Workspace::new("artifact-honesty");
    let mut cmd = campaign_cmd(&ws.driver(), &ws.root, &ws.out_dir(), None);
    cmd.env("HARNESS_SCOPE", "forge");
    cmd.env("HARNESS_ENUMERATE", "1");
    let (_, text) = run(&mut cmd);
    let artifact = fs::read_to_string(ws.out_dir().join("campaign_firewall.txt"))
        .unwrap_or_else(|e| panic!("read firewall artifact ({e}):\n{text}"));
    assert!(
        !artifact.contains("paid_provider_request_count"),
        "the artifact must not claim provider request counts it never measured:\n{artifact}"
    );
    assert!(
        !artifact.contains("submit_raw_send_count 0"),
        "the artifact must not claim a raw-send count it never measured:\n{artifact}"
    );
    assert!(
        artifact.contains("submit_no_send_authority=engine-gates"),
        "the artifact must state that no-submit rests on the engine DRY_RUN/SUBMIT_TX gates:\n{artifact}"
    );
    assert!(
        artifact.to_lowercase().contains("deferred"),
        "the artifact must record that counting-fake transport binding is DEFERRED:\n{artifact}"
    );
}
