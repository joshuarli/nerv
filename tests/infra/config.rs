use nerv::core::config::*;
use tempfile::TempDir;

#[test]
fn config_load_default_when_missing() {
    let tmp = TempDir::new().unwrap();
    let config = NervConfig::load(tmp.path());
    assert!(config.custom_providers.is_empty());
    assert!(config.default_model.is_none());
}

#[test]
fn config_save_and_load_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let mut config = NervConfig::default();
    config.custom_providers.push(CustomProviderConfig {
        name: "local".into(),
        base_url: "http://localhost:1234/v1".into(),
        api_key: None,
        models: vec![CustomModelConfig {
            id: "test-model".into(),
            name: Some("Test".into()),
            context_window: Some(32_000),
            reasoning: Some(true),
        }],
    });
    config.default_model = Some("local/test-model".into());

    config.save(tmp.path()).unwrap();
    let loaded = NervConfig::load(tmp.path());
    assert_eq!(loaded.custom_providers.len(), 1);
    assert_eq!(loaded.custom_providers[0].name, "local");
    assert_eq!(loaded.custom_providers[0].models[0].id, "test-model");
    assert_eq!(loaded.default_model.as_deref(), Some("local/test-model"));
}

#[test]
fn config_handles_corrupt_json() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("config.json"), "not json{{{").unwrap();
    let config = NervConfig::load(tmp.path());
    // Should return default, not panic
    assert!(config.custom_providers.is_empty());
}
