//! Compile-time embedded presentation pages and same-origin assets.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub enum Page {
    Dashboard,
    Login { error_code: Option<&'static str> },
    Setup,
}

pub struct Asset {
    pub body: &'static [u8],
    pub content_type: &'static str,
}

const DASHBOARD: &str = include_str!("web/dashboard.html");
const LOGIN: &str = include_str!("web/login.html");
const SETUP: &str = include_str!("web/setup.html");
const NIM_PROXY_ICON: &str = include_str!("web/icons/nim-proxy.svg");
const EN_US_SOURCE: &str = include_str!("web/locales/en-US.json");

pub const DEFAULT_LOCALE: &str = "en-US";
pub const INSTALLED_LOCALES: [&str; 1] = [DEFAULT_LOCALE];

#[derive(Debug, PartialEq, Eq)]
pub enum LocaleError {
    Invalid,
    NotInstalled(String),
}

/// Parse the deliberately small locale-v1 grammar and return its canonical
/// BCP 47 spelling. Extensions, variants and private-use subtags are outside
/// this frozen v1 contract.
pub fn canonical_locale(locale: &str) -> Result<String, LocaleError> {
    if locale.is_empty() || locale.trim() != locale || !locale.is_ascii() {
        return Err(LocaleError::Invalid);
    }
    let parts: Vec<&str> = locale.split('-').collect();
    let language = parts.first().copied().ok_or(LocaleError::Invalid)?;
    if !(matches!(language.len(), 2 | 3) && language.bytes().all(|byte| byte.is_ascii_alphabetic()))
    {
        return Err(LocaleError::Invalid);
    }

    let mut canonical = language.to_ascii_lowercase();
    let mut index = 1;
    if let Some(script) = parts.get(index).copied() {
        if script.len() == 4 && script.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            let mut chars = script.to_ascii_lowercase().into_bytes();
            chars[0] = chars[0].to_ascii_uppercase();
            canonical.push('-');
            canonical.push_str(std::str::from_utf8(&chars).expect("ASCII script"));
            index += 1;
        }
    }
    if let Some(region) = parts.get(index).copied() {
        let alpha = region.len() == 2 && region.bytes().all(|byte| byte.is_ascii_alphabetic());
        let numeric = region.len() == 3 && region.bytes().all(|byte| byte.is_ascii_digit());
        if !alpha && !numeric {
            return Err(LocaleError::Invalid);
        }
        canonical.push('-');
        if alpha {
            canonical.push_str(&region.to_ascii_uppercase());
        } else {
            canonical.push_str(region);
        }
        index += 1;
    }
    if index != parts.len() {
        return Err(LocaleError::Invalid);
    }
    Ok(canonical)
}

pub fn installed_locale(locale: &str) -> Result<String, LocaleError> {
    let canonical = canonical_locale(locale)?;
    if INSTALLED_LOCALES.contains(&canonical.as_str()) {
        Ok(canonical)
    } else {
        Err(LocaleError::NotInstalled(canonical))
    }
}

#[derive(Deserialize)]
struct AuthoringCatalog {
    locale: String,
    messages: BTreeMap<String, AuthoringMessage>,
}

#[derive(Deserialize)]
struct AuthoringMessage {
    en: String,
}

#[derive(Serialize)]
struct WireCatalog<'a> {
    locale: &'a str,
    messages: &'a BTreeMap<String, String>,
}

struct CatalogRegistry {
    operator: Box<[u8]>,
    public: Box<[u8]>,
}

static CATALOGS: OnceLock<CatalogRegistry> = OnceLock::new();

fn catalogs() -> &'static CatalogRegistry {
    CATALOGS.get_or_init(|| {
        let source: AuthoringCatalog =
            serde_json::from_str(EN_US_SOURCE).expect("embedded en-US catalog is valid");
        assert_eq!(
            source.locale, DEFAULT_LOCALE,
            "embedded catalog locale matches the compiled default"
        );

        let messages: BTreeMap<String, String> = source
            .messages
            .into_iter()
            .map(|(id, message)| (id, message.en))
            .collect();
        let public_messages: BTreeMap<String, String> = messages
            .iter()
            .filter(|(id, _)| {
                id.as_str() == "common.app_name"
                    || id.starts_with("login.")
                    || id.starts_with("setup.")
            })
            .map(|(id, message)| (id.clone(), message.clone()))
            .collect();

        CatalogRegistry {
            operator: serde_json::to_vec(&WireCatalog {
                locale: DEFAULT_LOCALE,
                messages: &messages,
            })
            .expect("embedded operator catalog serializes")
            .into_boxed_slice(),
            public: serde_json::to_vec(&WireCatalog {
                locale: DEFAULT_LOCALE,
                messages: &public_messages,
            })
            .expect("embedded public catalog serializes")
            .into_boxed_slice(),
        }
    })
}

pub fn page(page: Page) -> String {
    match page {
        Page::Dashboard => DASHBOARD.replace("<!-- nim-proxy-icon -->", NIM_PROXY_ICON),
        Page::Login { error_code } => {
            let code = match error_code {
                Some("invalid_credentials") => "invalid_credentials",
                _ => "",
            };
            LOGIN.replace("{{error_code}}", code)
        }
        Page::Setup => SETUP.to_owned(),
    }
}

pub fn public_asset(path: &str) -> Option<Asset> {
    match path {
        "/assets/public/public.css" => Some(Asset {
            body: include_bytes!("web/public.css"),
            content_type: "text/css; charset=utf-8",
        }),
        "/assets/public/setup.js" => Some(Asset {
            body: include_bytes!("web/setup.js"),
            content_type: "text/javascript; charset=utf-8",
        }),
        "/assets/public/login.js" => Some(Asset {
            body: include_bytes!("web/login.js"),
            content_type: "text/javascript; charset=utf-8",
        }),
        _ => None,
    }
}

pub fn operator_asset(path: &str) -> Option<Asset> {
    match path {
        "/assets/operator/operator.css" => Some(Asset {
            body: include_bytes!("web/operator.css"),
            content_type: "text/css; charset=utf-8",
        }),
        "/assets/operator/shared.js" => Some(Asset {
            body: include_bytes!("web/shared.js"),
            content_type: "text/javascript; charset=utf-8",
        }),
        "/assets/operator/dashboard.js" => Some(Asset {
            body: include_bytes!("web/dashboard.js"),
            content_type: "text/javascript; charset=utf-8",
        }),
        "/assets/operator/settings.js" => Some(Asset {
            body: include_bytes!("web/settings.js"),
            content_type: "text/javascript; charset=utf-8",
        }),
        _ => None,
    }
}

pub fn public_catalog(locale: &str) -> Option<Asset> {
    (locale == DEFAULT_LOCALE).then(|| Asset {
        body: &catalogs().public,
        content_type: "application/json",
    })
}

pub fn operator_catalog(locale: &str) -> Option<Asset> {
    (locale == DEFAULT_LOCALE).then(|| Asset {
        body: &catalogs().operator,
        content_type: "application/json",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_error_code_is_allowlisted() {
        let invalid = page(Page::Login {
            error_code: Some("invalid_credentials"),
        });
        assert!(invalid.contains(r#"data-error-code="invalid_credentials""#));

        let unknown = page(Page::Login {
            error_code: Some("<b>not trusted</b>"),
        });
        assert!(unknown.contains(r#"data-error-code="""#));
        assert!(!unknown.contains("<b>not trusted</b>"));
    }
}
