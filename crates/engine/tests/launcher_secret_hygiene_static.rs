use std::{
    fs,
    path::{Path, PathBuf},
};

const TRACKED_LAUNCHERS: &[&str] = &[
    "engine/scripts/run_full_opts.sh",
    "engine/scripts/dryrun_2h.sh",
];

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("engine/crates/engine has repo root ancestor")
        .to_path_buf()
}

fn read_required(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

#[test]
fn tracked_launchers_do_not_accept_sudo_passwords_from_environment() {
    for rel in TRACKED_LAUNCHERS {
        let text = read_required(rel);
        for forbidden in ["SUDO_PW", "sudo -S", "sudo_do()", "sudo_run()"] {
            assert!(
                !text.contains(forbidden),
                "{rel} must not contain env-fed sudo password handling: {forbidden}"
            );
        }
    }
}
