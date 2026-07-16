pub const PAGER_CLIENT_TYPE: &str = "ds-pager";
pub const HEADLESS_CLIENT_TYPE: &str = "ds-shell";

pub const PAGER_CLIENT_VERSION: &str = ds_version::VERSION;

/// `User-Agent` for pager-owned direct-to-`api.deepseek.com` clients (voice STT).
///
/// Matches the sampler's `ds-shell/<version> (os; arch)` shape so server-side
/// dashboards bucket voice traffic alongside chat / imagine requests.
pub fn client_user_agent() -> String {
    format!(
        "{}/{} ({}; {})",
        HEADLESS_CLIENT_TYPE,
        PAGER_CLIENT_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_user_agent_has_expected_shape() {
        // e.g. "ds-shell/1.2.3 (macos; aarch64)". The pieces are wire
        // contract for server-side UA parsing, so pin the exact shape.
        let ua = client_user_agent();
        assert_eq!(
            ua,
            format!(
                "ds-shell/{} ({}; {})",
                PAGER_CLIENT_VERSION,
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        );
    }
}
