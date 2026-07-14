pub mod api_errors;
pub mod pages;
pub mod serde_utils;
pub mod server;

#[derive(serde::Serialize)]
pub struct AppInfo {
    pub app: &'static str,
    pub version: &'static str,
    pub branch: &'static str,
    pub build: &'static str,
    pub commit: &'static str,
}

#[macro_export]
macro_rules! get_app_info {
    () => {{
        const APP: &str = env!("CARGO_CRATE_NAME");
        const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

        #[inline]
        fn git_branch() -> &'static str {
            option_env!("GIT_BRANCH").unwrap_or("n/a")
        }

        #[inline]
        fn git_version() -> &'static str {
            option_env!("GIT_VERSION").unwrap_or("n/a")
        }

        #[inline]
        fn git_commit() -> &'static str {
            option_env!("GIT_COMMIT").unwrap_or("n/a")
        }

        $crate::api_core::AppInfo {
            app: APP,
            version: PKG_VERSION,
            branch: git_branch(),
            build: git_version(),
            commit: git_commit(),
        }
    }};
}
