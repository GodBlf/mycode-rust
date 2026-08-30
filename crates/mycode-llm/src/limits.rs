use mycode_core::config::ProviderConfig;

pub fn max_output_tokens(provider: &ProviderConfig) -> u64 {
    provider
        .max_output_tokens
        .map(u64::from)
        .unwrap_or(if provider.thinking { 64_000 } else { 8_192 })
}

pub fn context_window(provider: &ProviderConfig, fetched: Option<u64>) -> u64 {
    if let Some(window) = provider.context_window {
        return u64::from(window);
    }
    if let Some(window) = fetched {
        return window;
    }
    built_in_context_window(&provider.model)
}

pub fn built_in_context_window(model: &str) -> u64 {
    let model = model.to_lowercase();
    let entries = [
        ("1m", 1_000_000),
        ("gpt-4.1", 1_000_000),
        ("gpt-4o", 128_000),
        ("gpt-4-turbo", 128_000),
        ("o1", 200_000),
        ("o3", 200_000),
        ("o4", 200_000),
        ("gpt-3.5", 16_385),
        ("claude", 200_000),
    ];
    entries
        .into_iter()
        .find(|(needle, _)| model.contains(needle))
        .map(|(_, window)| window)
        .unwrap_or(if model.contains("claude") {
            200_000
        } else {
            128_000
        })
}
