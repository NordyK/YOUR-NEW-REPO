use std::{fs, path::PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

#[test]
fn network_fine_configures_the_nic_before_requesting_timestamp_cmsgs() {
    let path = repo_root().join("engine/crates/engine-feed/src/relay_client.rs");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let start = source
        .find("if self.network_fine {")
        .expect("NETWORK_FINE socket configuration");
    let block = &source[start..];
    let discover = block
        .find("engine_sysopt::default_route_iface()")
        .expect("route interface discovery");
    let configure = block
        .find("engine_sysopt::configure_hardware_rx_timestamping(fd, &iface)")
        .expect("SIOCSHWTSTAMP configuration call");
    let enable_cmsgs = block
        .find("engine_sysopt::set_so_timestamping(fd)")
        .expect("SO_TIMESTAMPING call");

    assert!(
        discover < configure && configure < enable_cmsgs,
        "NIC HWTSTAMP configuration must precede SO_TIMESTAMPING; software cmsgs remain fallback"
    );
    assert!(
        block[..enable_cmsgs].contains("let hw_rx_configured ="),
        "hardware configuration success must be observed, not discarded"
    );
}
