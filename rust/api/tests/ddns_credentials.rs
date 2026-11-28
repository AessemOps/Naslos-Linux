//! The DDNS credential migration out of the apps namespace (FR-DNS-04).
//!
//! The decision itself is a pure function unit-tested in `naslos_api::ddns`;
//! these tests drive the thin IO around it with a stateful stub `kubectl`, so
//! the copy/verify/delete sequence and the "never delete an operator Secret"
//! guard are exercised against a fake API instead of only in the planner.

use std::sync::{Arc, Mutex};

use naslos_api::ddns::{self, Manager, Options};
use serde_json::Value;

static KUBECTL_LOCK: Mutex<()> = Mutex::new(());

const KEYS_NS: &str = "naslos-keys";
const APPS_NS: &str = "naslos-apps";
const ENTRY: &str = "abc";
const SECRET: &str = "naslos-ddns-abc";

fn secret_json(name: &str, namespace: &str, managed: bool) -> Value {
    use base64::Engine;
    let data = serde_json::json!({
        "username": base64::engine::general_purpose::STANDARD.encode("home"),
        "password": base64::engine::general_purpose::STANDARD.encode("s3cret-value"),
    });
    let labels = if managed {
        serde_json::json!({
            "app.kubernetes.io/managed-by": "naslos-api",
            "naslos.io/ddns-entry": ENTRY,
        })
    } else {
        serde_json::json!({})
    };
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": { "name": name, "namespace": namespace, "labels": labels },
        "data": data,
    })
}

/// A stateful stub `kubectl`. Secrets are one JSON file per
/// `<namespace>__<name>`; `create`/`apply` captures stdin, `delete` removes the
/// file and records the invocation, and a missing file is a NotFound error.
fn kubectl_stub(dir: &std::path::Path) -> std::path::PathBuf {
    let state = dir.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let script = format!(
        "#!/bin/sh\n\
         state='{state}'\n\
         printf '%s\\n' \"$*\" >> \"$state/calls.log\"\n\
         if [ \"$1\" = get ] && [ \"$2\" = secret ]; then\n\
           f=\"$state/$5__$3.json\"\n\
           if [ -f \"$f\" ]; then cat \"$f\"; exit 0; fi\n\
           echo \"Error from server (NotFound): secrets not found\" >&2\n\
           exit 1\n\
         fi\n\
         if [ \"$1\" = create ] || [ \"$1\" = apply ]; then\n\
           cat > \"$state/written.json\"\n\
           name=$(sed -n 's/.*\"name\":\"\\([^\"]*\\)\".*/\\1/p' \"$state/written.json\")\n\
           ns=$(sed -n 's/.*\"namespace\":\"\\([^\"]*\\)\".*/\\1/p' \"$state/written.json\")\n\
           cp \"$state/written.json\" \"$state/${{ns}}__${{name}}.json\" 2>/dev/null || true\n\
           echo '{{}}'\n\
           exit 0\n\
         fi\n\
         if [ \"$1\" = delete ] && [ \"$2\" = secret ]; then\n\
           printf '%s\\n' \"$*\" >> \"$state/deleted.log\"\n\
           rm -f \"$state/$5__$3.json\"\n\
           exit 0\n\
         fi\n\
         cat >/dev/null 2>&1\n\
         echo '{{}}'\n\
         exit 0\n",
        state = state.display()
    );
    let path = dir.join("kubectl");
    std::fs::write(&path, script).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

fn entry_with(secret_name: &str) -> ddns::Entry {
    ddns::Entry {
        id: ENTRY.into(),
        provider: "noip".into(),
        credentials_secret: secret_name.into(),
        ..Default::default()
    }
}

fn manager_with_stub(stub: &std::path::Path) -> Manager {
    std::env::set_var("KUBECTL_BIN", stub);
    Manager::new(Options {
        registry: Arc::new(naslos_api::providers::Registry::load("")),
        store_path: String::new(),
        keys_namespace: KEYS_NS.into(),
        apps_namespace: APPS_NS.into(),
        kube: Some(Arc::new(naslos_api::kube::Client::new(APPS_NS))),
        ip_sources: Vec::new(),
        ipv6_sources: Vec::new(),
        interval: std::time::Duration::ZERO,
        cooldown: std::time::Duration::ZERO,
    })
}

/// Seed a fake cluster with the given per-namespace Secrets and return the temp
/// directory. `KUBECTL_BIN` is process-global, so callers hold `KUBECTL_LOCK`.
fn fake_cluster(
    secret_name: &str,
    apps_secret: Option<Value>,
    keys_secret: Option<Value>,
) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("naslos-ddns-{}", ddns::new_id()));
    std::fs::create_dir_all(dir.join("state")).unwrap();
    let _ = kubectl_stub(&dir);
    for (namespace, secret) in [(APPS_NS, apps_secret), (KEYS_NS, keys_secret)] {
        if let Some(secret) = secret {
            let path = dir
                .join("state")
                .join(format!("{namespace}__{secret_name}.json"));
            std::fs::write(path, serde_json::to_vec(&secret).unwrap()).unwrap();
        }
    }
    dir
}

fn state_path(dir: &std::path::Path, namespace: &str, name: &str) -> std::path::PathBuf {
    dir.join("state").join(format!("{namespace}__{name}.json"))
}

fn read(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn restore_env(previous: Option<std::ffi::OsString>) {
    match previous {
        Some(value) => std::env::set_var("KUBECTL_BIN", value),
        None => std::env::remove_var("KUBECTL_BIN"),
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_managed_secret_is_copied_verified_and_the_apps_copy_removed() {
    let _guard = KUBECTL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("KUBECTL_BIN");
    let dir = fake_cluster(SECRET, Some(secret_json(SECRET, APPS_NS, true)), None);
    let manager = manager_with_stub(&dir.join("kubectl"));
    manager.store().put(entry_with(SECRET)).unwrap();
    manager.migrate_credentials().await;
    restore_env(previous);

    let written = read(&dir.join("state").join("written.json")).expect("a keys-namespace write");
    let object: Value = serde_json::from_str(&written).unwrap();
    assert_eq!(object["metadata"]["namespace"], KEYS_NS);
    assert_eq!(
        object["metadata"]["labels"]["app.kubernetes.io/managed-by"],
        "naslos-api"
    );
    assert_eq!(object["metadata"]["labels"]["naslos.io/ddns-entry"], ENTRY);
    assert_eq!(object["stringData"]["username"], "home");
    assert_eq!(object["stringData"]["password"], "s3cret-value");

    let deleted = read(&dir.join("state").join("deleted.log")).unwrap_or_default();
    assert!(
        deleted.contains(APPS_NS),
        "the apps copy must be deleted: {deleted}"
    );
    assert!(
        !state_path(&dir, APPS_NS, SECRET).exists(),
        "the apps copy must be gone"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn an_operator_secret_is_copied_but_never_deleted() {
    let _guard = KUBECTL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("KUBECTL_BIN");
    let operator = "operator-ddns-creds";
    let dir = fake_cluster(operator, Some(secret_json(operator, APPS_NS, false)), None);
    let manager = manager_with_stub(&dir.join("kubectl"));
    manager.store().put(entry_with(operator)).unwrap();
    manager.migrate_credentials().await;
    restore_env(previous);

    assert!(
        dir.join("state").join("written.json").exists(),
        "an operator Secret must still be copied into the keys namespace"
    );
    let written = read(&dir.join("state").join("written.json")).unwrap();
    let object: Value = serde_json::from_str(&written).unwrap();
    assert_eq!(object["metadata"]["namespace"], KEYS_NS);
    assert!(
        object["metadata"]["labels"]
            .get("app.kubernetes.io/managed-by")
            .is_none(),
        "an operator copy must not be marked Naslos-managed: {object}"
    );
    assert!(
        !dir.join("state").join("deleted.log").exists(),
        "an operator Secret must never be deleted"
    );
    assert!(state_path(&dir, APPS_NS, operator).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_keys_copy_wins_and_a_managed_apps_leftover_is_removed() {
    let _guard = KUBECTL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("KUBECTL_BIN");
    let dir = fake_cluster(
        SECRET,
        Some(secret_json(SECRET, APPS_NS, true)),
        Some(secret_json(SECRET, KEYS_NS, true)),
    );
    let manager = manager_with_stub(&dir.join("kubectl"));
    manager.store().put(entry_with(SECRET)).unwrap();
    manager.migrate_credentials().await;
    restore_env(previous);

    assert!(
        !dir.join("state").join("written.json").exists(),
        "an existing keys copy must never be overwritten"
    );
    assert!(dir.join("state").join("deleted.log").exists());
    assert!(state_path(&dir, KEYS_NS, SECRET).exists());
    assert!(!state_path(&dir, APPS_NS, SECRET).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_second_run_is_a_no_op() {
    let _guard = KUBECTL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("KUBECTL_BIN");
    let dir = fake_cluster(SECRET, Some(secret_json(SECRET, APPS_NS, true)), None);
    let manager = manager_with_stub(&dir.join("kubectl"));
    manager.store().put(entry_with(SECRET)).unwrap();
    manager.migrate_credentials().await;

    // A fresh manager on the same fake cluster: the apps copy is gone and the
    // keys copy exists, so nothing may be written or deleted.
    let _ = std::fs::remove_file(dir.join("state").join("written.json"));
    let _ = std::fs::remove_file(dir.join("state").join("deleted.log"));
    let manager = manager_with_stub(&dir.join("kubectl"));
    manager.store().put(entry_with(SECRET)).unwrap();
    manager.migrate_credentials().await;
    restore_env(previous);

    assert!(!dir.join("state").join("written.json").exists());
    assert!(!dir.join("state").join("deleted.log").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
