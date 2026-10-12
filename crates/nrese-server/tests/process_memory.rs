//! A separate, single-test process: changing the global fallback must not race other
//! server tests, including when run by cargo test rather than nextest.

use nrese_exec::memory::{process_limit, set_process_limit};
use nrese_reasoner::ReasonerService;
use nrese_server::ai::AiSuggestionService;
use nrese_server::{AppState, DeploymentPosture, ServerConfig};
use nrese_store::{StoreConfig, StoreService};

#[test]
fn opening_repositories_and_system_stores_preserves_the_startup_policy() {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            set_process_limit(self.0.unwrap_or(0));
        }
    }
    let _restore = Restore(process_limit());
    let mut config = ServerConfig {
        bind_address: "127.0.0.1:0".parse().unwrap(),
        deployment_posture: DeploymentPosture::OpenWorkbench,
        store: StoreConfig::in_memory(),
        reasoner: Default::default(),
        policy: Default::default(),
        ai: Default::default(),
        replication: Default::default(),
    };
    // A nondefault ceiling, and then explicit disable. Neither may be replaced by a
    // default policy while AppState opens the system/access store.
    for limit in [u64::MAX, 0] {
        config.store.process_memory_bytes = limit;
        config.install_process_memory_policy();
        let expected = (limit != 0).then_some(limit);
        let store = StoreService::new(config.store.clone()).unwrap();
        assert_eq!(process_limit(), expected);
        let _unrelated = StoreService::new(StoreConfig::in_memory()).unwrap();
        assert_eq!(process_limit(), expected);
        let _state = AppState::try_new(
            store,
            ReasonerService::new(config.reasoner.clone()),
            config.policy.clone(),
            AiSuggestionService::disabled(),
            config.deployment_posture,
        )
        .unwrap();
        assert_eq!(process_limit(), expected, "system store reset the ceiling");
    }
    // Disabling an embedded watch does not bypass the host's explicit fallback.
    config.store.process_memory_bytes = 1;
    config.install_process_memory_policy();
    let store = StoreService::new(StoreConfig {
        process_memory_bytes: 0,
        ..StoreConfig::in_memory()
    })
    .unwrap();
    assert_eq!(process_limit(), Some(1));
    if nrese_exec::memory::process_bytes().is_some() {
        assert!(
            store
                .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
                .is_err()
        );
    }
}
