use std::{
    fs,
    path::{Path, PathBuf},
};

const TERRAFORM_MAIN: &str = "engine/deploy/aws/terraform/main.tf";
const TERRAFORM_VARIABLES: &str = "engine/deploy/aws/terraform/variables.tf";
const LOGSHIP_SERVICE: &str = "engine/deploy/mantis-logship.service";
const LOGSHIP_TIMER: &str = "engine/deploy/mantis-logship.timer";
const SHIP_LOGS: &str = "engine/deploy/aws/ship-logs.sh";
const BOOTSTRAP_HOST: &str = "engine/deploy/aws/bootstrap-host.sh";
const SPOT_WATCHER: &str = "engine/deploy/aws/spot-interruption-watcher.sh";
const SPOT_WATCHER_SERVICE: &str = "engine/deploy/mantis-spot-interruption.service";
const DRYRUN_3H: &str = "engine/scripts/dryrun_3h.sh";
const ASSEMBLE_ARTIFACT: &str = "engine/scripts/assemble_aws_dryrun_artifact_dir.sh";
const RUN_ALL_HARNESSES: &str = "engine/deploy/aws/run-all-harnesses.sh";

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

fn has_spot_interruption_watcher(text: &str) -> bool {
    [
        "spot/instance-action",
        "spot/termination-time",
        "EC2 Spot Instance Interruption Warning",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

#[derive(Debug, Clone, Copy)]
struct InstalledPath<'a> {
    owner: &'a str,
    group: &'a str,
    mode: u16,
}

fn can_traverse_dir(user: &str, group: &str, path: InstalledPath<'_>) -> bool {
    (path.owner == user && path.mode & 0o100 != 0)
        || (path.group == group && path.mode & 0o010 != 0)
        || (path.mode & 0o001 != 0)
}

fn can_read_file(user: &str, group: &str, path: InstalledPath<'_>) -> bool {
    (path.owner == user && path.mode & 0o400 != 0)
        || (path.group == group && path.mode & 0o040 != 0)
        || (path.mode & 0o004 != 0)
}

#[test]
fn logship_service_user_can_read_terraform_installed_env_file() {
    let terraform = read_required(TERRAFORM_MAIN);
    let service = read_required(LOGSHIP_SERVICE);
    let ship_logs = read_required(SHIP_LOGS);

    assert!(
        service.contains("User=mantis") && service.contains("Group=mantis"),
        "mantis-logship.service must run as the mantis user/group"
    );
    assert!(
        ship_logs.contains("LOGSHIP_ENV=\"${LOGSHIP_ENV:-/etc/mantis/logship.env}\"")
            && ship_logs.contains("[ -f \"$LOGSHIP_ENV\" ] && . \"$LOGSHIP_ENV\""),
        "ship-logs.sh must source the Terraform-installed /etc/mantis/logship.env"
    );

    for required in [
        "install -d -o root -g mantis -m 750 /etc/mantis",
        "printf 'LOG_BUCKET=%s\\nAWS_REGION=%s\\n' '${local.log_bucket}' '${var.region}' > /etc/mantis/logship.env",
        "chown root:mantis /etc/mantis/secrets.env /etc/mantis/logship.env",
        "chmod 640 /etc/mantis/secrets.env /etc/mantis/logship.env",
    ] {
        assert!(
            terraform.contains(required),
            "Terraform user_data missing logship permission guard: {required}"
        );
    }

    let service_user = "mantis";
    let service_group = "mantis";
    let etc_mantis = InstalledPath {
        owner: "root",
        group: "mantis",
        mode: 0o750,
    };
    let logship_env = InstalledPath {
        owner: "root",
        group: "mantis",
        mode: 0o640,
    };

    assert!(
        can_traverse_dir(service_user, service_group, etc_mantis),
        "mantis user must be able to traverse /etc/mantis"
    );
    assert!(
        can_read_file(service_user, service_group, logship_env),
        "mantis user must be able to read /etc/mantis/logship.env"
    );
}

#[test]
fn aws_spot_dryrun_requires_an_installed_bounded_idempotent_interruption_flush() {
    let terraform_main = read_required(TERRAFORM_MAIN);
    let terraform_variables = read_required(TERRAFORM_VARIABLES);
    let logship_timer = read_required(LOGSHIP_TIMER);
    let ship_logs = read_required(SHIP_LOGS);
    let bootstrap = read_required(BOOTSTRAP_HOST);
    let watcher = read_required(SPOT_WATCHER);
    let watcher_service = read_required(SPOT_WATCHER_SERVICE);
    let dryrun_3h = read_required(DRYRUN_3H);
    let assembler = read_required(ASSEMBLE_ARTIFACT);
    let run_all = read_required(RUN_ALL_HARNESSES);

    assert!(
        has_spot_interruption_watcher(&watcher),
        "the dedicated watcher must consume an EC2 Spot interruption notice"
    );
    for required in [
        "latest/api/token",
        "X-aws-ec2-metadata-token",
        "spot/instance-action",
        "flock",
        "ship-logs.sh",
        "--final",
        "timeout",
        "MANTIS_SPOT_STOP_TIMEOUT_SECS",
        "mantis-engine.service",
        "mantis-relay-sidecar.service",
        "mantis-relay.service",
        "mantis-logship.timer",
        "mantis-logship.service",
    ] {
        assert!(watcher.contains(required), "Spot watcher lost {required}");
    }
    assert!(
        watcher_service.contains("ExecStart=/opt/mantis/engine/bin/spot-interruption-watcher.sh")
            && watcher_service.contains("Restart=always")
            && watcher_service.contains("After=network-online.target"),
        "Spot watcher must be an installed, restartable system service"
    );
    assert!(
        bootstrap.contains("spot-interruption-watcher.sh")
            && bootstrap.contains("mantis-spot-interruption.service")
            && bootstrap.contains("systemctl enable --now mantis-spot-interruption.service"),
        "Terraform/bootstrap surface must install and enable the interruption watcher"
    );
    assert!(
        logship_timer.contains("OnUnitActiveSec=1min"),
        "periodic durability interval must bound ordinary pre-notice loss to one minute"
    );

    for required in [
        "spot_type",
        "instance_interruption_behavior",
        "instance_initiated_shutdown_behavior",
        "interruption behavior is always terminate",
        "pull or ship logs before any teardown",
        "metadata_options",
        "http_tokens                 = \"required\"",
        "http_put_response_hop_limit = 1",
        "aws_s3_bucket\" \"logs\"",
        "s3:PutObject",
        "Resource = [\"${aws_s3_bucket.logs[0].arn}/mantis/*\"]",
    ] {
        assert!(
            terraform_main.contains(required),
            "Terraform AWS config missing Spot/log-durability boundary: {required}"
        );
    }

    assert!(
        terraform_variables.contains("enable_log_shipping")
            && terraform_variables.contains("default     = true")
            && terraform_variables.contains("checksum-verified final dry-run flush"),
        "durable external shipping must be on by default for a Spot dry run"
    );

    for required in [
        "aws s3 sync",
        "aws s3api put-object",
        "aws s3api head-object",
        "aws s3api get-object",
        "http://169.254.169.254/latest/api/token",
        "http://169.254.169.254/latest/meta-data/instance-id",
        "--exclude '*.tmp'",
        "--exclude '*.lock'",
    ] {
        assert!(
            ship_logs.contains(required),
            "ship-logs helper missing durability surface: {required}"
        );
    }

    assert!(
        run_all.contains("real AWS dry-run artifact are separate external")
            && run_all.contains("not represented here as ignored cargo tests"),
        "run-all harness must not claim real AWS artifact durability as a local/static gate"
    );

    assert!(
        assembler.contains("RUN_STARTED_UTC")
            && assembler.contains("RUN_FINISHED_UTC")
            && assembler.contains("artifact_manifest.json"),
        "artifact assembler must require explicit completed-run timestamps and manifest output"
    );

    // Keep the laptop-only helpers honest: the AWS interruption authority belongs
    // to the dedicated watcher and must not be implied by an unrelated harness.
    assert!(
        !has_spot_interruption_watcher(&dryrun_3h)
            && !has_spot_interruption_watcher(&assembler)
            && !has_spot_interruption_watcher(&run_all),
        "Spot interruption handling must have one dedicated owner"
    );
}

#[test]
fn terraform_dryrun_is_exactly_one_instance_with_a_mandatory_guest_ttl() {
    let terraform = read_required(TERRAFORM_MAIN);
    let variables = read_required(TERRAFORM_VARIABLES);

    assert!(
        terraform.contains("resource \"aws_spot_instance_request\" \"engine\"")
            && terraform.contains("count = var.use_spot ? 1 : 0")
            && terraform.contains("resource \"aws_instance\" \"engine\"")
            && terraform.contains("count = var.use_spot ? 0 : 1"),
        "the dry-run/live choice must materialize exactly one compute instance"
    );
    for forbidden in [
        "resource \"aws_autoscaling_group\"",
        "resource \"aws_ec2_fleet\"",
        "resource \"aws_spot_fleet_request\"",
    ] {
        assert!(
            !terraform.contains(forbidden),
            "one-box Terraform must not contain {forbidden}"
        );
    }

    assert!(
        variables.contains("variable \"dryrun_ttl_minutes\"")
            && variables.contains("dryrun_ttl_minutes must be")
            && terraform.contains("var.dryrun_ttl_minutes")
            && terraform.contains("mantis-dryrun-ttl.timer")
            && terraform.contains("OnCalendar=$ttl_deadline")
            && terraform.contains("Persistent=true")
            && terraform.contains("shutdown -h"),
        "one-time Spot is not a duration limit: the guest needs a mandatory, reboot-persistent validated dry-run TTL"
    );
    assert!(
        terraform.contains("spot_type")
            && terraform.contains("\"one-time\"")
            && terraform.contains("instance_interruption_behavior")
            && terraform.contains("instance_initiated_shutdown_behavior")
            && terraform.matches("= \"terminate\"").count() >= 2,
        "the single Spot request must fail closed to termination"
    );
}
