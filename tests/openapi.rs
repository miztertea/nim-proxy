//! `openapi.json` is generated from the handlers, so it can only be right by
//! construction — this test is what makes it *stay* right. It regenerates the
//! spec and compares it to the committed file; drift fails the build.
//!
//! Regenerate after changing a handler, a response type, or the crate
//! version:
//!
//! ```sh
//! UPDATE_OPENAPI=1 cargo test --test openapi
//! ```
//!
//! CI runs exactly that and then `git diff --exit-code -- openapi.json`, so a
//! PR cannot merge a spec that disagrees with the code.

use std::path::PathBuf;

fn assert_global_security(spec: &serde_json::Value) {
    let global = spec["security"].as_array().expect("document security");
    assert_eq!(
        global,
        &[
            serde_json::json!({"session_cookie": []}),
            serde_json::json!({"basic_auth": []}),
        ],
        "route-contract:openapi-security: the /api/* routes inherit exactly session-cookie or Basic credentials"
    );
}

fn spec_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("openapi.json")
}

#[test]
fn committed_spec_matches_the_code() {
    let generated = nim_proxy::openapi_json();
    let path = spec_path();

    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::write(&path, &generated).expect("write openapi.json");
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}\nRegenerate it with `UPDATE_OPENAPI=1 cargo test --test openapi`",
            path.display()
        )
    });
    assert_eq!(
        committed, generated,
        "openapi.json is out of date with the handlers.\n\
         Regenerate it with `UPDATE_OPENAPI=1 cargo test --test openapi` and commit the result."
    );
}

/// A spec nothing can consume is worse than none: assert the invariants a
/// generator client actually needs, rather than trusting the file blindly.
#[test]
fn spec_is_usable() {
    let spec: serde_json::Value =
        serde_json::from_str(&nim_proxy::openapi_json()).expect("the spec is JSON");

    assert_eq!(spec["openapi"], "3.1.0");
    assert_eq!(spec["info"]["version"], env!("CARGO_PKG_VERSION"));

    let paths = spec["paths"].as_object().expect("paths");
    let operation_count: usize = paths
        .values()
        .map(|item| item.as_object().expect("path item").len())
        .sum();
    assert_eq!(
        operation_count, 16,
        "14 /api/* operations + the 2 setup operations"
    );
    assert_eq!(
        paths["/api/locale-bootstrap"]["get"]["security"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "GET /api/locale-bootstrap is intentionally public"
    );
    for omitted in [
        "/",
        "/dash",
        "/health",
        "/login",
        "/logout",
        "/metrics",
        "/v1/{*path}",
    ] {
        assert!(
            !paths.contains_key(omitted),
            "{omitted}: non-control-plane or upstream-owned route must stay outside this spec"
        );
    }

    for (path, item) in paths {
        for (method, op) in item.as_object().expect("path item") {
            let label = format!("{method} {path}");
            assert!(
                op["responses"]["200"].is_object(),
                "{label}: no documented success response"
            );
            assert!(
                op["tags"].as_array().is_some_and(|t| !t.is_empty()),
                "{label}: untagged"
            );
            // The setup routes sit outside the session guard and say so with
            // an explicit empty `security` list; /api/* routes carry none of
            // their own and so inherit the document-level requirement.
            if path == "/api/locale-bootstrap" {
                assert_eq!(
                    op["security"].as_array().map(Vec::len),
                    Some(0),
                    "{label}: locale bootstrap is public by design"
                );
            } else if path.starts_with("/api/") {
                assert!(
                    op.get("security").is_none(),
                    "{label}: an /api/* route must inherit the document security requirement"
                );
            } else {
                assert_eq!(
                    op["security"].as_array().map(Vec::len),
                    Some(0),
                    "{label}: the setup routes are unauthenticated by design and must say so"
                );
            }
        }
    }

    for (path, item) in paths {
        for (method, op) in item.as_object().expect("path item") {
            for (status, response) in op["responses"].as_object().expect("responses") {
                if status.starts_with('2') {
                    continue;
                }
                assert_eq!(
                    response["content"]["application/json"]["schema"]["$ref"],
                    "#/components/schemas/ApiError",
                    "{method} {path} {status}: every JSON API rejection uses ApiError"
                );
            }
        }
    }

    // The document-level requirement the /api/* routes inherit.
    assert_global_security(&spec);

    // Every schema a response references must actually be in components.
    let schemas = spec["components"]["schemas"]
        .as_object()
        .expect("components.schemas");
    for name in [
        "ApiError",
        "ConfigResponse",
        "DashboardResponse",
        "LocaleBootstrap",
        "OkResponse",
    ] {
        assert!(schemas.contains_key(name), "{name} is missing");
    }
    let security = spec["components"]["securitySchemes"]
        .as_object()
        .expect("securitySchemes");
    assert_eq!(security["session_cookie"]["type"], "apiKey");
    assert_eq!(security["session_cookie"]["in"], "cookie");
    assert_eq!(security["session_cookie"]["name"], "nimproxy_session");
    assert_eq!(security["basic_auth"]["type"], "http");
    assert_eq!(security["basic_auth"]["scheme"], "basic");
}

#[test]
fn locale_bootstrap_schema_is_typed() {
    let spec: serde_json::Value =
        serde_json::from_str(&nim_proxy::openapi_json()).expect("the spec is JSON");
    assert_eq!(
        spec["paths"]["/api/locale-bootstrap"]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/LocaleBootstrap",
        "locale bootstrap success must reference its Rust response type"
    );
    let schema = &spec["components"]["schemas"]["LocaleBootstrap"];
    assert_eq!(schema["type"], "object");
    assert_eq!(
        schema["required"],
        serde_json::json!(["installed_locales", "server_default"])
    );
    assert_eq!(schema["properties"]["installed_locales"]["type"], "array");
    assert_eq!(
        schema["properties"]["installed_locales"]["items"]["type"],
        "string"
    );
    assert_eq!(schema["properties"]["server_default"]["type"], "string");
}

fn resolve_schema<'a>(
    spec: &'a serde_json::Value,
    schema: &'a serde_json::Value,
) -> &'a serde_json::Value {
    let Some(reference) = schema.get("$ref").and_then(serde_json::Value::as_str) else {
        return schema;
    };
    let name = reference
        .strip_prefix("#/components/schemas/")
        .expect("local component schema reference");
    &spec["components"]["schemas"][name]
}

fn schema_variants<'a>(
    spec: &'a serde_json::Value,
    schema: &'a serde_json::Value,
) -> Vec<&'a serde_json::Value> {
    let schema = resolve_schema(spec, schema);
    for keyword in ["oneOf", "anyOf"] {
        if let Some(variants) = schema.get(keyword).and_then(serde_json::Value::as_array) {
            return variants
                .iter()
                .flat_map(|variant| schema_variants(spec, variant))
                .collect();
        }
    }
    vec![schema]
}

fn nullable_string(schema: &serde_json::Value) -> bool {
    schema.get("nullable") == Some(&serde_json::Value::Bool(true))
        && schema.get("type") == Some(&serde_json::Value::String("string".into()))
        || schema
            .get("type")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|types| {
                types.as_slice()
                    == [
                        serde_json::Value::String("string".into()),
                        serde_json::Value::String("null".into()),
                    ]
                    || types.as_slice()
                        == [
                            serde_json::Value::String("null".into()),
                            serde_json::Value::String("string".into()),
                        ]
            })
}

#[test]
fn locale_server_default_openapi_is_typed() {
    let spec: serde_json::Value =
        serde_json::from_str(&nim_proxy::openapi_json()).expect("the spec is JSON");
    let operation = &spec["paths"]["/api/settings/locale"]["post"];
    assert!(
        operation.is_object(),
        "locale-openapi: missing POST /api/settings/locale"
    );
    assert!(
        operation.get("security").is_none(),
        "locale-openapi: admin route must inherit operator authentication"
    );
    assert_eq!(
        operation["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/SetServerLocale"
    );
    let request = &spec["components"]["schemas"]["SetServerLocale"];
    assert_eq!(request["type"], "object");
    assert_eq!(request["required"], serde_json::json!(["locale"]));
    assert_eq!(request["properties"]["locale"]["type"], "string");
    for (status, schema) in [
        ("200", "OkResponse"),
        ("400", "ApiError"),
        ("401", "ApiError"),
        ("403", "ApiError"),
        ("422", "ApiError"),
    ] {
        assert_eq!(
            operation["responses"][status]["content"]["application/json"]["schema"]["$ref"],
            format!("#/components/schemas/{schema}"),
            "locale-openapi: POST /api/settings/locale {status}"
        );
    }
}

#[test]
fn locale_account_openapi_preserves_password_and_adds_preference_actions() {
    let spec: serde_json::Value =
        serde_json::from_str(&nim_proxy::openapi_json()).expect("the spec is JSON");
    let operation = &spec["paths"]["/api/settings/account"]["post"];
    let summary = operation["summary"].as_str().unwrap_or_default();
    assert!(
        summary.contains("password") && summary.contains("locale preference"),
        "locale-openapi: account summary must describe both actions: {summary:?}"
    );
    let ok_description = operation["responses"]["200"]["description"]
        .as_str()
        .unwrap_or_default();
    assert!(
        ok_description.contains("fresh session cookie")
            && ok_description.contains("locale preference")
            && ok_description.contains("without changing the session"),
        "locale-openapi: 200 must distinguish password and locale success: {ok_description:?}"
    );
    let bad_request_description = operation["responses"]["400"]["description"]
        .as_str()
        .unwrap_or_default();
    for code in [
        "weak_password",
        "invalid_action",
        "invalid_locale",
        "locale_not_installed",
        "invalid_config",
    ] {
        assert!(
            bad_request_description.contains(code),
            "locale-openapi: 400 description must name {code}: {bad_request_description:?}"
        );
    }
    assert_eq!(
        operation["responses"]["422"]["description"],
        "Invalid JSON request body"
    );
    assert_eq!(
        operation["responses"]["422"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ApiError"
    );

    let request = &operation["requestBody"]["content"]["application/json"]["schema"];
    let variants = schema_variants(&spec, request);

    let password = variants.iter().copied().find(|schema| {
        schema["required"] == serde_json::json!(["current_password", "new_password"])
            && schema["properties"]["current_password"]["type"] == "string"
            && schema["properties"]["new_password"]["type"] == "string"
    });
    assert!(
        password.is_some(),
        "locale-openapi: account must retain the exact password-change body"
    );

    let locale = variants.iter().copied().find(|schema| {
        schema["required"] == serde_json::json!(["action", "locale"])
            && schema["properties"]["action"]["enum"] == serde_json::json!(["locale"])
            && nullable_string(&schema["properties"]["locale"])
    });
    let locale = locale.expect("locale-openapi: account must type the locale set/clear action");
    assert_eq!(
        locale["additionalProperties"], false,
        "locale-openapi: locale action is an exact closed object"
    );
}

#[test]
fn locale_config_response_openapi_fields_are_typed_and_ascii_positioned() {
    let spec: serde_json::Value =
        serde_json::from_str(&nim_proxy::openapi_json()).expect("the spec is JSON");
    let config = &spec["components"]["schemas"]["ConfigResponse"];
    assert_eq!(
        config["required"],
        serde_json::json!([
            "client_keys",
            "locale",
            "mode",
            "nim_keys",
            "pool",
            "role",
            "username"
        ]),
        "locale-openapi: ConfigResponse required fields stay ASCII-positioned"
    );
    assert!(
        nullable_string(&config["properties"]["locale"]),
        "locale-openapi: current-user locale is string|null"
    );

    let server = &spec["components"]["schemas"]["ServerSettings"];
    assert_eq!(
        server["required"],
        serde_json::json!([
            "base_url",
            "dashboard",
            "default_locale",
            "governor",
            "history",
            "limits"
        ]),
        "locale-openapi: ServerSettings required fields stay ASCII-positioned"
    );
    assert_eq!(server["properties"]["default_locale"]["type"], "string");
}

#[test]
#[should_panic(expected = "route-contract:openapi-security")]
fn global_security_self_test_names_wrong_requirement() {
    let mut spec: serde_json::Value =
        serde_json::from_str(&nim_proxy::openapi_json()).expect("generated OpenAPI");
    spec["security"] = serde_json::json!([{"wrong_scheme": []}]);
    assert_global_security(&spec);
}
