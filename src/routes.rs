//! Fixed route paths and test-only descriptive trust-boundary metadata.
//!
//! The constants below are consumed by the live router in `lib.rs`. The
//! contract inventory is deliberately test-only: it describes those live
//! registrations and must never become a second handler registry.

pub const ROOT: &str = "/";
pub const DASH: &str = "/dash";
pub const METRICS: &str = "/metrics";
pub const ASSET_PUBLIC_CSS: &str = "/assets/public/public.css";
pub const ASSET_PUBLIC_SETUP_JS: &str = "/assets/public/setup.js";
pub const ASSET_PUBLIC_LOGIN_JS: &str = "/assets/public/login.js";
// Axum does not support a parameter plus a suffix in one segment; the handler
// accepts only `{locale}.json`, so the externally visible contract remains
// `/assets/public/locales/{locale}.json`.
pub const ASSET_PUBLIC_LOCALE: &str = "/assets/public/locales/{locale_file}";
pub const ASSET_OPERATOR_CSS: &str = "/assets/operator/operator.css";
pub const ASSET_OPERATOR_SHARED_JS: &str = "/assets/operator/shared.js";
pub const ASSET_OPERATOR_DASHBOARD_JS: &str = "/assets/operator/dashboard.js";
pub const ASSET_OPERATOR_SETTINGS_JS: &str = "/assets/operator/settings.js";
pub const ASSET_OPERATOR_LOCALE: &str = "/assets/operator/locales/{locale_file}";

pub const API_PREFIX: &str = "/api";
pub const API_LOCALE_BOOTSTRAP: &str = "/locale-bootstrap";
pub const API_DASHBOARD: &str = "/dashboard";
pub const API_DASHBOARD_NOW: &str = "/dashboard/now";
pub const API_CONFIG: &str = "/config";
pub const API_SETTINGS_NIM_KEYS: &str = "/settings/nim-keys";
pub const API_SETTINGS_CLIENTS: &str = "/settings/clients";
pub const API_SETTINGS_UPSTREAM: &str = "/settings/upstream";
pub const API_SETTINGS_LIMITS: &str = "/settings/limits";
pub const API_SETTINGS_HISTORY: &str = "/settings/history";
pub const API_SETTINGS_GOVERNOR: &str = "/settings/governor";
pub const API_SETTINGS_USERS: &str = "/settings/users";
pub const API_SETTINGS_ACCOUNT: &str = "/settings/account";
pub const API_SETTINGS_LOCALE: &str = "/settings/locale";
pub const API_SETTINGS_VALIDATE_KEY: &str = "/settings/validate-key";

pub const HEALTH: &str = "/health";
pub const LOGIN: &str = "/login";
pub const LOGOUT: &str = "/logout";
pub const SETUP: &str = "/setup";
pub const SETUP_VALIDATE_KEY: &str = "/setup/validate-key";
pub const V1_WILDCARD: &str = "/v1/{*path}";

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Access {
    Client,
    Public,
    OperatorAny,
    OperatorAdmin,
    OperatorSuperuser,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Always,
    PreSetup,
    PostSetup,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub struct RouteContract {
    pub access: Access,
    pub method: &'static str,
    pub openapi: bool,
    pub path: &'static str,
    pub phase: Phase,
    pub probe_path: &'static str,
}

#[cfg(test)]
const ROUTES: &[RouteContract] = &[
    RouteContract {
        access: Access::Public,
        method: "GET",
        openapi: false,
        path: ASSET_PUBLIC_CSS,
        phase: Phase::Always,
        probe_path: ASSET_PUBLIC_CSS,
    },
    RouteContract {
        access: Access::Public,
        method: "GET",
        openapi: false,
        path: ASSET_PUBLIC_SETUP_JS,
        phase: Phase::Always,
        probe_path: ASSET_PUBLIC_SETUP_JS,
    },
    RouteContract {
        access: Access::Public,
        method: "GET",
        openapi: false,
        path: ASSET_PUBLIC_LOGIN_JS,
        phase: Phase::Always,
        probe_path: ASSET_PUBLIC_LOGIN_JS,
    },
    RouteContract {
        access: Access::Public,
        method: "GET",
        openapi: false,
        path: "/assets/public/locales/{locale}.json",
        phase: Phase::Always,
        probe_path: "/assets/public/locales/en-US.json",
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "GET",
        openapi: false,
        path: ASSET_OPERATOR_CSS,
        phase: Phase::PostSetup,
        probe_path: ASSET_OPERATOR_CSS,
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "GET",
        openapi: false,
        path: ASSET_OPERATOR_SHARED_JS,
        phase: Phase::PostSetup,
        probe_path: ASSET_OPERATOR_SHARED_JS,
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "GET",
        openapi: false,
        path: ASSET_OPERATOR_DASHBOARD_JS,
        phase: Phase::PostSetup,
        probe_path: ASSET_OPERATOR_DASHBOARD_JS,
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "GET",
        openapi: false,
        path: ASSET_OPERATOR_SETTINGS_JS,
        phase: Phase::PostSetup,
        probe_path: ASSET_OPERATOR_SETTINGS_JS,
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "GET",
        openapi: false,
        path: "/assets/operator/locales/{locale}.json",
        phase: Phase::PostSetup,
        probe_path: "/assets/operator/locales/en-US.json",
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "GET",
        openapi: false,
        path: ROOT,
        phase: Phase::PostSetup,
        probe_path: ROOT,
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "GET",
        openapi: false,
        path: DASH,
        phase: Phase::PostSetup,
        probe_path: DASH,
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "GET",
        openapi: false,
        path: METRICS,
        phase: Phase::PostSetup,
        probe_path: METRICS,
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "GET",
        openapi: true,
        path: "/api/dashboard",
        phase: Phase::PostSetup,
        probe_path: "/api/dashboard",
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "GET",
        openapi: true,
        path: "/api/dashboard/now",
        phase: Phase::PostSetup,
        probe_path: "/api/dashboard/now",
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "GET",
        openapi: true,
        path: "/api/config",
        phase: Phase::PostSetup,
        probe_path: "/api/config",
    },
    RouteContract {
        access: Access::Public,
        method: "GET",
        openapi: true,
        path: "/api/locale-bootstrap",
        phase: Phase::Always,
        probe_path: "/api/locale-bootstrap",
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "POST",
        openapi: true,
        path: "/api/settings/nim-keys",
        phase: Phase::PostSetup,
        probe_path: "/api/settings/nim-keys",
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "POST",
        openapi: true,
        path: "/api/settings/clients",
        phase: Phase::PostSetup,
        probe_path: "/api/settings/clients",
    },
    RouteContract {
        access: Access::OperatorAdmin,
        method: "POST",
        openapi: true,
        path: "/api/settings/limits",
        phase: Phase::PostSetup,
        probe_path: "/api/settings/limits",
    },
    RouteContract {
        access: Access::OperatorAdmin,
        method: "POST",
        openapi: true,
        path: "/api/settings/history",
        phase: Phase::PostSetup,
        probe_path: "/api/settings/history",
    },
    RouteContract {
        access: Access::OperatorAdmin,
        method: "POST",
        openapi: true,
        path: "/api/settings/governor",
        phase: Phase::PostSetup,
        probe_path: "/api/settings/governor",
    },
    RouteContract {
        access: Access::OperatorAdmin,
        method: "POST",
        openapi: true,
        path: "/api/settings/users",
        phase: Phase::PostSetup,
        probe_path: "/api/settings/users",
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "POST",
        openapi: true,
        path: "/api/settings/validate-key",
        phase: Phase::PostSetup,
        probe_path: "/api/settings/validate-key",
    },
    RouteContract {
        access: Access::OperatorAdmin,
        method: "POST",
        openapi: true,
        path: "/api/settings/upstream",
        phase: Phase::PostSetup,
        probe_path: "/api/settings/upstream",
    },
    RouteContract {
        access: Access::OperatorAny,
        method: "POST",
        openapi: true,
        path: "/api/settings/account",
        phase: Phase::PostSetup,
        probe_path: "/api/settings/account",
    },
    RouteContract {
        access: Access::OperatorAdmin,
        method: "POST",
        openapi: true,
        path: "/api/settings/locale",
        phase: Phase::PostSetup,
        probe_path: "/api/settings/locale",
    },
    RouteContract {
        access: Access::Public,
        method: "GET",
        openapi: false,
        path: HEALTH,
        phase: Phase::Always,
        probe_path: HEALTH,
    },
    RouteContract {
        access: Access::Public,
        method: "GET",
        openapi: false,
        path: LOGIN,
        phase: Phase::Always,
        probe_path: LOGIN,
    },
    RouteContract {
        access: Access::Public,
        method: "POST",
        openapi: false,
        path: LOGIN,
        phase: Phase::Always,
        probe_path: LOGIN,
    },
    RouteContract {
        access: Access::Public,
        method: "POST",
        openapi: false,
        path: LOGOUT,
        phase: Phase::Always,
        probe_path: LOGOUT,
    },
    RouteContract {
        access: Access::Public,
        method: "GET",
        openapi: false,
        path: SETUP,
        phase: Phase::PreSetup,
        probe_path: SETUP,
    },
    RouteContract {
        access: Access::Public,
        method: "POST",
        openapi: true,
        path: SETUP,
        phase: Phase::PreSetup,
        probe_path: SETUP,
    },
    RouteContract {
        access: Access::Public,
        method: "POST",
        openapi: true,
        path: SETUP_VALIDATE_KEY,
        phase: Phase::PreSetup,
        probe_path: SETUP_VALIDATE_KEY,
    },
    RouteContract {
        access: Access::Client,
        method: "ANY",
        openapi: false,
        path: V1_WILDCARD,
        phase: Phase::PostSetup,
        probe_path: "/v1/chat/completions",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const REGISTERED_API_PATHS: [&str; 14] = [
        API_LOCALE_BOOTSTRAP,
        API_DASHBOARD,
        API_DASHBOARD_NOW,
        API_CONFIG,
        API_SETTINGS_NIM_KEYS,
        API_SETTINGS_CLIENTS,
        API_SETTINGS_UPSTREAM,
        API_SETTINGS_LIMITS,
        API_SETTINGS_HISTORY,
        API_SETTINGS_GOVERNOR,
        API_SETTINGS_USERS,
        API_SETTINGS_ACCOUNT,
        API_SETTINGS_LOCALE,
        API_SETTINGS_VALIDATE_KEY,
    ];

    fn assert_registered_api_paths(registered_api_paths: &[&str]) {
        assert_eq!(
            registered_api_paths.len(),
            14,
            "route-contract:registration: every nested /api registration must be reconciled"
        );
        for registered_path in registered_api_paths {
            let full_path = format!("{API_PREFIX}{registered_path}");
            assert!(
                ROUTES
                    .iter()
                    .any(|route| route.path == full_path && route.openapi),
                "route-contract:registration: {API_PREFIX} + {registered_path} is absent from the inventory"
            );
        }
    }

    #[test]
    #[should_panic(expected = "route-contract:registration")]
    fn registration_self_test_names_relative_path_drift() {
        let mut drifted = REGISTERED_API_PATHS;
        drifted[0] = "/drifted-dashboard";
        assert_registered_api_paths(&drifted);
    }

    #[test]
    fn inventory_agrees_with_generated_openapi() {
        let spec: serde_json::Value =
            serde_json::from_str(&crate::api::openapi_json()).expect("generated OpenAPI JSON");
        let paths = spec["paths"].as_object().expect("OpenAPI paths");

        assert_eq!(ROUTES.len(), 34, "route-contract:inventory");
        assert_eq!(
            ROUTES
                .iter()
                .filter(|route| route.path.starts_with("/assets/"))
                .count(),
            9,
            "route-contract:assets"
        );
        assert_registered_api_paths(&REGISTERED_API_PATHS);
        let mut method_paths = HashSet::new();
        let mut probes = HashSet::new();
        for route in ROUTES {
            assert!(
                method_paths.insert((route.method, route.path)),
                "route-contract:inventory: duplicate {} {}",
                route.method,
                route.path
            );
            assert!(
                probes.insert((route.method, route.probe_path)),
                "route-contract:probe: duplicate {} {}",
                route.method,
                route.probe_path
            );
        }
        assert_eq!(
            ROUTES
                .iter()
                .filter(|route| route.phase == Phase::Always)
                .count(),
            9,
            "route-contract:phase: health, both login methods, logout, bootstrap, and public assets are phase-independent"
        );
        assert_eq!(
            ROUTES
                .iter()
                .filter(|route| route.phase == Phase::PreSetup)
                .count(),
            3,
            "route-contract:phase: setup GET and both setup POST routes"
        );
        assert_eq!(
            ROUTES
                .iter()
                .filter(|route| route.phase == Phase::PostSetup)
                .count(),
            22,
            "route-contract:phase: operator, operator assets, and client routes"
        );
        assert!(
            !ROUTES
                .iter()
                .any(|route| route.access == Access::OperatorSuperuser),
            "route-contract:auth: superuser is an invariant, not extra endpoint power"
        );

        for route in ROUTES.iter().filter(|route| route.openapi) {
            let method = route.method.to_ascii_lowercase();
            let operation = paths
                .get(route.path)
                .and_then(|item| item.get(&method))
                .unwrap_or_else(|| {
                    panic!(
                        "route-contract:openapi: missing {} {}",
                        route.method, route.path
                    )
                });
            match route.access {
                Access::Public => assert_eq!(
                    operation["security"].as_array().map(Vec::len),
                    Some(0),
                    "route-contract:openapi: public {} {} must waive security",
                    route.method,
                    route.path
                ),
                Access::OperatorAny | Access::OperatorAdmin | Access::OperatorSuperuser => {
                    assert!(
                        operation.get("security").is_none(),
                        "route-contract:openapi: operator {} {} must inherit security",
                        route.method,
                        route.path
                    );
                }
                Access::Client => {
                    panic!("route-contract:openapi: /v1 is upstream-owned and must stay omitted")
                }
            }
        }

        for (path, item) in paths {
            for method in item.as_object().expect("OpenAPI path item").keys() {
                assert!(
                    ROUTES.iter().any(|route| {
                        route.openapi
                            && route.path == path
                            && route.method.eq_ignore_ascii_case(method)
                    }),
                    "route-contract:openapi: undocumented inventory decision for {method} {path}"
                );
            }
        }

        assert!(
            ROUTES
                .iter()
                .all(|route| { !route.path.contains('{') || !route.probe_path.contains('{') }),
            "route-contract:probe: parameterized routes need fixture-backed probe paths"
        );
    }

    #[test]
    fn locale_preference_routes_have_the_locked_boundaries() {
        let server_default = ROUTES
            .iter()
            .find(|route| route.method == "POST" && route.path == "/api/settings/locale");
        let server_default =
            server_default.expect("route-contract:locale: missing POST /api/settings/locale");
        assert_eq!(
            server_default.access,
            Access::OperatorAdmin,
            "route-contract:locale: server default must be admin-only"
        );
        assert_eq!(server_default.phase, Phase::PostSetup);
        assert!(server_default.openapi);

        let own_preference = ROUTES
            .iter()
            .find(|route| route.method == "POST" && route.path == "/api/settings/account");
        let own_preference =
            own_preference.expect("route-contract:locale: missing account preference route");
        assert_eq!(own_preference.access, Access::OperatorAny);
        assert_eq!(own_preference.phase, Phase::PostSetup);
        assert!(own_preference.openapi);
    }
}
