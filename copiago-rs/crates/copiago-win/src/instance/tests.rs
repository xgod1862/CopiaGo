use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

const CHILD_NAMESPACE: &str = "COPIAGO_T_WIN_02_CHILD_NAMESPACE";
const CHILD_MINIMIZED: &str = "COPIAGO_T_WIN_02_CHILD_MINIMIZED";

#[test]
fn isolated_launches_activate_the_primary_only_when_requested() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let namespace = format!("win02-{}-{nanos}", std::process::id());
    let primary = Instance::acquire_isolated(&namespace, false)
        .unwrap()
        .unwrap();
    assert!(!primary.take_activation().unwrap());

    launch_child(&namespace, true);
    assert!(!primary.take_activation().unwrap());
    launch_child(&namespace, false);
    assert!(primary.take_activation().unwrap());
    assert!(!primary.take_activation().unwrap());

    drop(primary);
    assert!(
        Instance::acquire_isolated(&namespace, false)
            .unwrap()
            .is_some()
    );
}

fn launch_child(namespace: &str, minimized: bool) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "instance::tests::child_instance"])
        .env(CHILD_NAMESPACE, namespace)
        .env(CHILD_MINIMIZED, if minimized { "1" } else { "0" })
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
#[ignore = "launched by isolated_launches_activate_the_primary_only_when_requested"]
fn child_instance() {
    let Ok(namespace) = std::env::var(CHILD_NAMESPACE) else {
        return;
    };
    let minimized = std::env::var(CHILD_MINIMIZED).unwrap() == "1";
    assert!(
        Instance::acquire_isolated(&namespace, minimized)
            .unwrap()
            .is_none()
    );
}
