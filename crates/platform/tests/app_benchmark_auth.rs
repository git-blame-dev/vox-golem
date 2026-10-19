#![cfg(all(feature = "app-benchmark", unix))]

use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::{fs, io::Write, process::Command};
use voxgolem_platform::{
    chatgpt_auth,
    custom_openai::{CustomOpenAiClient, CustomOpenAiConfig, CustomOpenAiError, CustomOpenAiModel},
};

#[test]
fn offline_benchmark_auth_owner_preserves_generation_fence() {
    const CHILD: &str = "VOXGOLEM_TEST_BENCH_AUTH_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "offline_benchmark_auth_owner_preserves_generation_fence",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("VOXGOLEM_BENCH_MODE", "offline")
            .env("VOXGOLEM_BENCH_MCP_ENDPOINT", "http://127.0.0.1:9/mcp")
            .env_remove("VOXGOLEM_BENCH_PERMITS")
            .status()
            .expect("run isolated benchmark environment");
        assert!(
            status.success(),
            "isolated benchmark auth regression failed"
        );
        return;
    }

    assert!(matches!(
        CustomOpenAiClient::new(CustomOpenAiConfig::default()),
        Err(CustomOpenAiError::InvalidConfig)
    ));
    let config = CustomOpenAiConfig {
        endpoint: "http://127.0.0.1:9/v1/responses".into(),
        model: CustomOpenAiModel::LunaLow,
        ..CustomOpenAiConfig::default()
    };
    let client = CustomOpenAiClient::new(config.clone()).expect("loopback generation config");
    client
        .with_reused_transport(config)
        .expect("loopback reused generation config");
    assert!(matches!(
        client.with_reused_transport(CustomOpenAiConfig::default()),
        Err(CustomOpenAiError::InvalidConfig)
    ));

    let directory = std::path::PathBuf::from("/tmp/opencode")
        .join(format!("benchmark-auth-regression-{}", std::process::id()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .expect("create private synthetic auth directory");
    let path = directory.join("auth.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .expect("create private synthetic auth file");
    file.write_all(
        br#"{"openai":{"access":"synthetic-benchmark-access","expires":4102444800000}}"#,
    )
    .expect("write synthetic unmanaged credentials");
    drop(file);

    // Fresh unmanaged credentials need neither refresh HTTP nor generation.
    // Poll owner initialization outside a Tokio context: on failure its newly
    // constructed runtime must be dropped before reporting the auth error.
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let mut fresh = std::pin::pin!(chatgpt_auth::ensure_fresh(&path));
    let result = match fresh.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(result) => result,
        Poll::Pending => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(fresh),
    };
    fs::remove_file(&path).expect("remove synthetic credentials");
    fs::remove_dir(directory).expect("remove private test directory");
    assert!(result.is_ok(), "fresh unmanaged auth failed: {result:?}");
}
