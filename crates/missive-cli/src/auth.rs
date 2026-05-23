//! Authentication input resolution for outbound missive CLI requests.
//!
//! This module turns non-secret config references and explicit CLI inputs into
//! validated HTTP headers. Raw token values are kept in memory only long enough
//! to build an outbound request and are never persisted to SQLite or rendered in
//! structured output.

use std::collections::BTreeMap;

use missive_a2a::AuthHeaders;
use missive_core::{
    AuthRefConfig as ConfigAuthRefConfig, AuthRefKind as ConfigAuthRefKind, LoadedConfig,
    MissiveError, Result,
};
use missive_store::{AgentRecord, AuthRefKind as StoreAuthRefKind, AuthRefRecord, Store};

use crate::GlobalArgs;

/// Resolves auth headers for an outbound request to a registered agent.
pub(crate) fn auth_headers_for_agent(
    store: &Store,
    agent: &AgentRecord,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
) -> Result<AuthHeaders> {
    let mut headers = AuthHeaders::new();

    if let Some(auth_ref_name) = agent.auth_ref_name.as_deref() {
        let auth_ref = store.get_auth_ref(auth_ref_name)?.ok_or_else(|| {
            MissiveError::auth(format!(
                "agent {:?} references missing auth ref {auth_ref_name:?}",
                agent.alias.as_str()
            ))
            .with_help("Add the auth ref to the loaded config or update the agent registry entry.")
        })?;
        apply_auth_ref(&mut headers, &auth_ref, environment)?;
    }

    apply_global_auth_headers(&mut headers, globals, environment)?;

    Ok(headers)
}

/// Resolves auth headers for a config-seeded agent without mutating the local store.
pub(crate) fn auth_headers_for_config_agent(
    loaded_config: &LoadedConfig,
    agent_alias: &str,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
) -> Result<AuthHeaders> {
    let mut headers = AuthHeaders::new();
    let agent = loaded_config
        .config
        .agents
        .get(agent_alias)
        .ok_or_else(|| MissiveError::config(format!("agent {agent_alias:?} is not configured")))?;

    if let Some(auth_ref_name) = agent.auth_ref.as_deref() {
        let auth_ref = loaded_config
            .config
            .auth_refs
            .get(auth_ref_name)
            .ok_or_else(|| {
                MissiveError::auth(format!(
                    "configured agent {agent_alias:?} references missing auth ref"
                ))
                .with_help("Add the auth ref to the loaded config or remove the agent auth_ref.")
            })?;
        apply_config_auth_ref(&mut headers, auth_ref_name, auth_ref, environment)?;
    }

    apply_global_auth_headers(&mut headers, globals, environment)?;

    Ok(headers)
}

fn apply_global_auth_headers(
    headers: &mut AuthHeaders,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
) -> Result<()> {
    if let Some(env_name) = globals.bearer_token_env.as_deref() {
        validate_env_var_name("--bearer-token-env", env_name)?;
        let token = required_env_secret("--bearer-token-env", env_name, environment)?;
        headers.insert("Authorization", format!("Bearer {token}"))?;
    }

    for value in &globals.headers {
        let (name, header_value) = parse_header_arg(value)?;
        headers.insert(name, header_value)?;
    }

    Ok(())
}

fn apply_auth_ref(
    headers: &mut AuthHeaders,
    auth_ref: &AuthRefRecord,
    environment: &BTreeMap<String, String>,
) -> Result<()> {
    let secret = match auth_ref.kind {
        StoreAuthRefKind::Env => {
            let env_name = auth_ref.env_var.as_deref().ok_or_else(|| {
                MissiveError::auth(format!(
                    "auth ref {:?} is environment-backed but has no env_var",
                    auth_ref.name
                ))
                .with_help("Update [auth_refs] in the loaded config.")
            })?;
            required_env_secret(
                &format!("auth ref {:?}", auth_ref.name),
                env_name,
                environment,
            )?
        }
        StoreAuthRefKind::Keyring => {
            let service = auth_ref.keyring_service.as_deref().ok_or_else(|| {
                MissiveError::auth(format!(
                    "auth ref {:?} is keyring-backed but has no keyring_service",
                    auth_ref.name
                ))
                .with_help("Update [auth_refs] in the loaded config.")
            })?;
            let account = auth_ref.keyring_account.as_deref().ok_or_else(|| {
                MissiveError::auth(format!(
                    "auth ref {:?} is keyring-backed but has no keyring_account",
                    auth_ref.name
                ))
                .with_help("Update [auth_refs] in the loaded config.")
            })?;
            resolve_keyring_secret(&auth_ref.name, service, account)?
        }
        StoreAuthRefKind::External => {
            return Err(MissiveError::auth(format!(
                "auth ref {:?} uses external secret storage that this CLI cannot resolve yet",
                auth_ref.name
            ))
            .with_help("Use an env or keyring auth ref, or pass --bearer-token-env/--header."));
        }
    };

    insert_auth_secret(
        headers,
        &auth_ref.header_name,
        auth_ref.scheme.as_deref(),
        secret,
    )
}

fn apply_config_auth_ref(
    headers: &mut AuthHeaders,
    name: &str,
    auth_ref: &ConfigAuthRefConfig,
    environment: &BTreeMap<String, String>,
) -> Result<()> {
    let secret = match auth_ref.kind {
        ConfigAuthRefKind::Env => {
            let env_name = auth_ref.env.as_deref().ok_or_else(|| {
                MissiveError::auth("configured environment auth ref has no env variable")
                    .with_help("Update [auth_refs] in the loaded config.")
            })?;
            required_env_secret("configured auth ref", env_name, environment)?
        }
        ConfigAuthRefKind::Keyring => {
            let service = auth_ref.keyring_service.as_deref().ok_or_else(|| {
                MissiveError::auth("configured keyring auth ref has no keyring_service")
                    .with_help("Update [auth_refs] in the loaded config.")
            })?;
            let account = auth_ref.keyring_account.as_deref().ok_or_else(|| {
                MissiveError::auth("configured keyring auth ref has no keyring_account")
                    .with_help("Update [auth_refs] in the loaded config.")
            })?;
            resolve_keyring_secret(name, service, account)?
        }
    };

    insert_auth_secret(headers, &auth_ref.header, Some(&auth_ref.scheme), secret)
}

fn insert_auth_secret(
    headers: &mut AuthHeaders,
    header_name: &str,
    scheme: Option<&str>,
    secret: String,
) -> Result<()> {
    let header_value = if let Some(scheme) = scheme.filter(|value| !value.is_empty()) {
        format!("{scheme} {secret}")
    } else {
        secret
    };
    headers.insert(header_name, header_value)
}

pub(crate) fn required_env_secret(
    source: &str,
    env_name: &str,
    environment: &BTreeMap<String, String>,
) -> Result<String> {
    validate_env_var_name(source, env_name)?;
    let value = environment.get(env_name).ok_or_else(|| {
        MissiveError::auth(format!(
            "environment variable {env_name:?} required by {source} is not set"
        ))
        .with_help("Set the environment variable before retrying; do not put the token in config or SQLite.")
    })?;
    if value.trim().is_empty() {
        return Err(MissiveError::auth(format!(
            "environment variable {env_name:?} required by {source} is empty"
        ))
        .with_help("Set the environment variable to a non-empty token before retrying."));
    }
    Ok(value.clone())
}

fn parse_header_arg(value: &str) -> Result<(&str, &str)> {
    let Some((name, header_value)) = value.split_once(':') else {
        return Err(MissiveError::validation(
            "--header must use Name:Value syntax with a non-empty HTTP header name and value",
        ));
    };
    let name = name.trim();
    let header_value = header_value.trim();
    if name.is_empty() || header_value.is_empty() {
        return Err(MissiveError::validation(
            "--header must include a non-empty HTTP header name and value",
        ));
    }
    Ok((name, header_value))
}

pub(crate) fn validate_env_var_name(source: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(MissiveError::validation(format!(
            "{source} must name a non-empty environment variable"
        )));
    }
    let bytes = value.as_bytes();
    if !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') {
        return Err(MissiveError::validation(format!(
            "{source} environment variable name must start with an ASCII letter or underscore"
        ))
        .with_help("Pass an environment variable name, not the token value."));
    }
    if bytes
        .iter()
        .any(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_'))
    {
        return Err(MissiveError::validation(format!(
            "{source} environment variable name must contain only ASCII letters, digits, and underscore"
        ))
        .with_help("Pass an environment variable name, not the token value."));
    }
    Ok(())
}

#[cfg(feature = "native-keyring")]
fn resolve_keyring_secret(auth_ref_name: &str, service: &str, account: &str) -> Result<String> {
    let entry = keyring::Entry::new(service, account).map_err(|error| {
        MissiveError::auth(format!(
            "could not open platform keyring entry for auth ref {auth_ref_name:?}"
        ))
        .with_source(error)
        .with_help("Ensure this missive build supports the platform keyring and that the service/account names are valid.")
    })?;
    let token = entry.get_password().map_err(|error| {
        MissiveError::auth(format!(
            "keyring token for auth ref {auth_ref_name:?} is unavailable"
        ))
        .with_source(error)
        .with_help("Provision the token in the platform keyring or choose an env-backed auth ref.")
    })?;
    if token.trim().is_empty() {
        return Err(MissiveError::auth(format!(
            "keyring token for auth ref {auth_ref_name:?} is empty"
        ))
        .with_help("Update the platform keyring entry to contain a non-empty token."));
    }
    Ok(token)
}

#[cfg(not(feature = "native-keyring"))]
fn resolve_keyring_secret(auth_ref_name: &str, _service: &str, _account: &str) -> Result<String> {
    Err(MissiveError::auth(format!(
        "auth ref {auth_ref_name:?} is keyring-backed, but this missive binary was built without native keyring support"
    ))
    .with_help("Rebuild with the native-keyring feature or use an env-backed auth ref."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_header_syntax_does_not_echo_secret_on_errors() {
        let hidden = "value-hidden-in-output";
        let error = parse_header_arg(&format!("Authorization {hidden}"))
            .expect_err("missing colon should fail");
        let rendered = error.to_string();

        assert!(rendered.contains("--header"));
        assert!(!rendered.contains(hidden));
    }

    #[test]
    fn bearer_env_missing_uses_auth_category_without_secret_value() {
        let error =
            required_env_secret("--bearer-token-env", "MISSIVE_TEST_TOKEN", &BTreeMap::new())
                .expect_err("missing env should fail");

        assert_eq!(error.code(), "missive::auth");
        assert!(error.to_string().contains("MISSIVE_TEST_TOKEN"));
    }

    #[test]
    fn bearer_env_validation_rejects_injected_token_values_without_echoing_them() {
        let hidden = "Bearer value-hidden-in-output";
        let error = validate_env_var_name("--bearer-token-env", hidden)
            .expect_err("raw token values are not env var names");

        assert_eq!(error.code(), "missive::validation");
        let rendered = error.to_string();
        assert!(rendered.contains("environment variable name"));
        assert!(!rendered.contains("value-hidden-in-output"));
    }
}
