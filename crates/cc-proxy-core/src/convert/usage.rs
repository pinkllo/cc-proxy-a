use crate::types::claude::Usage;

pub fn derive_claude_usage(
    estimated_total_input_tokens: u32,
    upstream_total_input_tokens: u32,
    upstream_output_tokens: u32,
    upstream_cached_input_tokens: Option<u32>,
) -> Usage {
    if upstream_total_input_tokens == 0 && upstream_output_tokens == 0 {
        return Usage::default();
    }

    let total_input_tokens =
        resolve_total_input_tokens(estimated_total_input_tokens, upstream_total_input_tokens);
    let cached_input_tokens = resolve_cached_input_tokens(
        total_input_tokens,
        upstream_total_input_tokens,
        upstream_cached_input_tokens.unwrap_or(0),
    );

    Usage {
        input_tokens: total_input_tokens.saturating_sub(cached_input_tokens),
        output_tokens: upstream_output_tokens,
        cache_read_input_tokens: (cached_input_tokens > 0).then_some(cached_input_tokens),
        // Preserve the upstream's raw prompt_tokens for accurate cost estimation.
        // The upstream API bills based on this number, not our adjusted input_tokens.
        upstream_input_tokens: Some(upstream_total_input_tokens),
    }
}

fn resolve_total_input_tokens(
    estimated_total_input_tokens: u32,
    upstream_total_input_tokens: u32,
) -> u32 {
    if estimated_total_input_tokens > 0 && upstream_total_input_tokens > 0 {
        estimated_total_input_tokens.min(upstream_total_input_tokens)
    } else if estimated_total_input_tokens > 0 {
        estimated_total_input_tokens
    } else {
        upstream_total_input_tokens
    }
}

fn resolve_cached_input_tokens(
    total_input_tokens: u32,
    upstream_total_input_tokens: u32,
    upstream_cached_input_tokens: u32,
) -> u32 {
    if total_input_tokens == 0
        || upstream_total_input_tokens == 0
        || upstream_cached_input_tokens == 0
    {
        return 0;
    }

    let cache_ratio = upstream_cached_input_tokens as f64 / upstream_total_input_tokens as f64;
    ((total_input_tokens as f64 * cache_ratio).round() as u32).min(total_input_tokens)
}

#[cfg(test)]
mod tests {
    use super::derive_claude_usage;

    #[test]
    fn splits_cached_tokens_out_of_input_tokens() {
        let usage = derive_claude_usage(10, 500, 100, Some(300));

        assert_eq!(usage.input_tokens, 4);
        assert_eq!(usage.output_tokens, 100);
        assert_eq!(usage.cache_read_input_tokens, Some(6));
        assert_eq!(usage.upstream_input_tokens, Some(500));
    }

    #[test]
    fn leaves_input_tokens_untouched_without_cache_details() {
        let usage = derive_claude_usage(10, 50, 25, None);

        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 25);
        assert_eq!(usage.cache_read_input_tokens, None);
        assert_eq!(usage.upstream_input_tokens, Some(50));
    }

    #[test]
    fn clamps_cached_tokens_to_total_input() {
        let usage = derive_claude_usage(8, 10, 20, Some(100));

        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, Some(8));
        assert_eq!(usage.upstream_input_tokens, Some(10));
    }
}
