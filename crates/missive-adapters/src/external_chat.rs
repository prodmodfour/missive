//! Feature-gated external chat adapter placeholders.
//!
//! These types intentionally do **not** implement live network integrations.
//! They give missive a compileable registry boundary for future Discord, Slack,
//! Telegram, Matrix, and Email adapters while keeping heavy SDKs and real
//! platform credentials out of the current workspace.

use missive_core::{Metadata, MissiveError, Result};
use serde::{Deserialize, Serialize};

use crate::{
    Adapter, AdapterAcknowledgement, AdapterContext, AdapterDefinition, AdapterExternalIdentity,
    AdapterIdentity, AdapterOutboundUpdate, AdapterRegistry,
};

/// Feature-gated placeholder adapter kind for Discord.
pub const DISCORD_ADAPTER_KIND: &str = "discord";

/// Feature-gated placeholder adapter kind for Slack.
pub const SLACK_ADAPTER_KIND: &str = "slack";

/// Feature-gated placeholder adapter kind for Telegram.
pub const TELEGRAM_ADAPTER_KIND: &str = "telegram";

/// Feature-gated placeholder adapter kind for Matrix.
pub const MATRIX_ADAPTER_KIND: &str = "matrix";

/// Feature-gated placeholder adapter kind for Email.
pub const EMAIL_ADAPTER_KIND: &str = "email";

const DISCORD_SECRETS: &[&str] = &["bot_token_auth_ref", "interaction_public_key"];
const DISCORD_PERMISSIONS: &[&str] = &[
    "message content intent or explicit mention/command routing",
    "read selected channels or threads",
    "send messages and optional slash-command responses",
];
const DISCORD_BEHAVIORS: &[&str] = &[
    "gateway events and interaction callbacks have separate acknowledgement deadlines",
    "guild/channel/thread ids are operationally sensitive source identifiers",
    "platform rate limits must be honored per route and globally",
];

const SLACK_SECRETS: &[&str] = &["bot_token_auth_ref", "signing_secret_auth_ref"];
const SLACK_PERMISSIONS: &[&str] = &[
    "app_mentions:read or command/event subscriptions for inbound routing",
    "chat:write for outbound updates",
    "channel or direct-message history scopes only when explicitly needed",
];
const SLACK_BEHAVIORS: &[&str] = &[
    "event delivery retries require idempotent acknowledgement handling",
    "workspace/team/channel ids must map to source sessions",
    "response URLs and platform tokens must never be persisted raw",
];

const TELEGRAM_SECRETS: &[&str] = &["bot_token_auth_ref", "webhook_secret_auth_ref"];
const TELEGRAM_PERMISSIONS: &[&str] = &[
    "bot command/message receipt for selected chats",
    "sendMessage or equivalent outbound update permissions",
    "optional webhook management outside missive unless a later ticket adds it",
];
const TELEGRAM_BEHAVIORS: &[&str] = &[
    "privacy mode changes whether group messages are visible",
    "long polling and webhooks have different offset/retry semantics",
    "chat ids and message ids are source identifiers, not agent memory",
];

const MATRIX_SECRETS: &[&str] = &["access_token_auth_ref", "homeserver_url"];
const MATRIX_PERMISSIONS: &[&str] = &[
    "join/read selected rooms",
    "send room messages for outbound updates",
    "manage sync tokens and device ids as local runtime state",
];
const MATRIX_BEHAVIORS: &[&str] = &[
    "homeserver federation can delay or reorder room events",
    "end-to-end encrypted rooms need a future explicit crypto design",
    "room/user/event ids must be treated as operationally sensitive",
];

const EMAIL_SECRETS: &[&str] = &[
    "smtp_auth_ref",
    "imap_or_graph_auth_ref",
    "oauth_refresh_auth_ref",
];
const EMAIL_PERMISSIONS: &[&str] = &[
    "read from an explicitly configured mailbox or folder",
    "send mail through an explicitly configured relay/provider",
    "store message-id/thread correlation without storing credentials",
];
const EMAIL_BEHAVIORS: &[&str] = &[
    "polling intervals are slower and less interactive than chat platforms",
    "MIME bodies and attachments require size limits and sanitization",
    "reply threading, bounces, and spam filtering affect acknowledgement state",
];

/// External chat/platform adapters that have compileable stubs in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalChatPlatform {
    /// Discord bot/gateway or interactions adapter placeholder.
    Discord,
    /// Slack app/event adapter placeholder.
    Slack,
    /// Telegram bot adapter placeholder.
    Telegram,
    /// Matrix room adapter placeholder.
    Matrix,
    /// Email mailbox adapter placeholder.
    Email,
}

impl ExternalChatPlatform {
    /// Returns every external platform for which missive documents a placeholder.
    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Discord,
            Self::Slack,
            Self::Telegram,
            Self::Matrix,
            Self::Email,
        ]
    }

    /// Resolves a platform from a registry adapter kind.
    #[must_use]
    pub fn from_kind(kind: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .find(|platform| platform.kind() == kind)
    }

    /// Adapter kind used in `[adapters.<name>].kind`.
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Discord => DISCORD_ADAPTER_KIND,
            Self::Slack => SLACK_ADAPTER_KIND,
            Self::Telegram => TELEGRAM_ADAPTER_KIND,
            Self::Matrix => MATRIX_ADAPTER_KIND,
            Self::Email => EMAIL_ADAPTER_KIND,
        }
    }

    /// Human-readable platform name.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Discord => "Discord",
            Self::Slack => "Slack",
            Self::Telegram => "Telegram",
            Self::Matrix => "Matrix",
            Self::Email => "Email",
        }
    }

    /// Cargo feature that enables this platform's registry factory stub.
    #[must_use]
    pub const fn cargo_feature(self) -> &'static str {
        match self {
            Self::Discord => "adapter-discord",
            Self::Slack => "adapter-slack",
            Self::Telegram => "adapter-telegram",
            Self::Matrix => "adapter-matrix",
            Self::Email => "adapter-email",
        }
    }

    /// Whether this platform's feature is enabled for the current build.
    #[must_use]
    pub const fn stub_feature_enabled(self) -> bool {
        match self {
            Self::Discord => cfg!(feature = "adapter-discord"),
            Self::Slack => cfg!(feature = "adapter-slack"),
            Self::Telegram => cfg!(feature = "adapter-telegram"),
            Self::Matrix => cfg!(feature = "adapter-matrix"),
            Self::Email => cfg!(feature = "adapter-email"),
        }
    }

    /// Static roadmap metadata for this platform placeholder.
    #[must_use]
    pub const fn info(self) -> ExternalChatPlatformInfo {
        match self {
            Self::Discord => ExternalChatPlatformInfo {
                platform: self,
                kind: DISCORD_ADAPTER_KIND,
                display_name: "Discord",
                cargo_feature: "adapter-discord",
                required_secret_refs: DISCORD_SECRETS,
                required_permissions: DISCORD_PERMISSIONS,
                platform_behaviors: DISCORD_BEHAVIORS,
            },
            Self::Slack => ExternalChatPlatformInfo {
                platform: self,
                kind: SLACK_ADAPTER_KIND,
                display_name: "Slack",
                cargo_feature: "adapter-slack",
                required_secret_refs: SLACK_SECRETS,
                required_permissions: SLACK_PERMISSIONS,
                platform_behaviors: SLACK_BEHAVIORS,
            },
            Self::Telegram => ExternalChatPlatformInfo {
                platform: self,
                kind: TELEGRAM_ADAPTER_KIND,
                display_name: "Telegram",
                cargo_feature: "adapter-telegram",
                required_secret_refs: TELEGRAM_SECRETS,
                required_permissions: TELEGRAM_PERMISSIONS,
                platform_behaviors: TELEGRAM_BEHAVIORS,
            },
            Self::Matrix => ExternalChatPlatformInfo {
                platform: self,
                kind: MATRIX_ADAPTER_KIND,
                display_name: "Matrix",
                cargo_feature: "adapter-matrix",
                required_secret_refs: MATRIX_SECRETS,
                required_permissions: MATRIX_PERMISSIONS,
                platform_behaviors: MATRIX_BEHAVIORS,
            },
            Self::Email => ExternalChatPlatformInfo {
                platform: self,
                kind: EMAIL_ADAPTER_KIND,
                display_name: "Email",
                cargo_feature: "adapter-email",
                required_secret_refs: EMAIL_SECRETS,
                required_permissions: EMAIL_PERMISSIONS,
                platform_behaviors: EMAIL_BEHAVIORS,
            },
        }
    }
}

/// Static roadmap metadata for one external chat/platform placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalChatPlatformInfo {
    /// Platform variant.
    pub platform: ExternalChatPlatform,
    /// Adapter registry kind.
    pub kind: &'static str,
    /// Human-readable platform name.
    pub display_name: &'static str,
    /// Cargo feature that enables this platform's registry factory stub.
    pub cargo_feature: &'static str,
    /// Secret reference names a future adapter is expected to resolve.
    pub required_secret_refs: &'static [&'static str],
    /// Permissions/scopes a future adapter should request narrowly.
    pub required_permissions: &'static [&'static str],
    /// Platform-specific delivery/session behaviors a future adapter must handle.
    pub platform_behaviors: &'static [&'static str],
}

/// Returns the external chat stub platforms enabled by Cargo features.
#[must_use]
pub fn enabled_external_chat_stub_platforms() -> Vec<ExternalChatPlatform> {
    ExternalChatPlatform::all()
        .into_iter()
        .filter(|platform| platform.stub_feature_enabled())
        .collect()
}

/// Minimal placeholder adapter for future external chat integrations.
///
/// The stub can be created through the registry when its feature is enabled, and
/// it can map identities for tests/documentation. Runtime start, update
/// delivery, and acknowledgements deliberately return configuration errors so a
/// user cannot mistake the placeholder for a live platform adapter.
#[derive(Debug, Clone)]
pub struct ExternalChatStubAdapter {
    definition: AdapterDefinition,
    platform: ExternalChatPlatform,
}

impl ExternalChatStubAdapter {
    /// Creates a stub adapter by resolving `definition.kind` to a known platform.
    pub fn new(definition: AdapterDefinition) -> Result<Self> {
        let platform = ExternalChatPlatform::from_kind(&definition.kind).ok_or_else(|| {
            MissiveError::config(format!(
                "external chat stub adapter does not support kind {:?}",
                definition.kind
            ))
            .with_help("Use one of discord, slack, telegram, matrix, or email.")
        })?;
        Self::new_for_platform(definition, platform)
    }

    /// Creates a stub adapter for an explicitly selected platform.
    pub fn new_for_platform(
        definition: AdapterDefinition,
        platform: ExternalChatPlatform,
    ) -> Result<Self> {
        if definition.kind != platform.kind() {
            return Err(MissiveError::config(format!(
                "{} adapter stub cannot be created for adapter kind {:?}",
                platform.display_name(),
                definition.kind
            ))
            .with_help(format!(
                "Set adapter kind to {:?} for this placeholder.",
                platform.kind()
            )));
        }
        Ok(Self {
            definition,
            platform,
        })
    }

    /// Returns the platform represented by this placeholder.
    #[must_use]
    pub const fn platform(&self) -> ExternalChatPlatform {
        self.platform
    }

    /// Returns static roadmap metadata for this placeholder.
    #[must_use]
    pub const fn platform_info(&self) -> ExternalChatPlatformInfo {
        self.platform.info()
    }

    fn unsupported_error(&self, operation: &str) -> MissiveError {
        MissiveError::config(format!(
            "{} adapter is a feature-gated stub; {operation} is not implemented",
            self.platform.display_name()
        ))
        .with_help(
            "The feature only exposes registry and identity placeholders. Use stdio, file-drop, or http adapters today, and keep future platform credentials in auth refs, environment variables, or keyrings.",
        )
    }

    fn validate_same_adapter(&self, adapter_name: &str, operation: &str) -> Result<()> {
        if adapter_name != self.definition.name {
            return Err(MissiveError::validation(format!(
                "{} adapter stub cannot {operation} for adapter {:?}",
                self.definition.name, adapter_name
            ))
            .with_help("Deliver updates and acknowledgements only to the adapter that mapped the source identity."));
        }
        Ok(())
    }
}

impl Adapter for ExternalChatStubAdapter {
    fn definition(&self) -> &AdapterDefinition {
        &self.definition
    }

    fn start(&mut self, _context: AdapterContext) -> Result<()> {
        Err(self.unsupported_error("runtime start"))
    }

    fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    fn map_identity(&self, external: AdapterExternalIdentity) -> Result<AdapterIdentity> {
        let AdapterExternalIdentity {
            provider_user_id,
            provider_channel_id,
            display_name,
            metadata,
        } = external;
        let source_id = provider_channel_id.as_ref().map_or_else(
            || provider_user_id.clone(),
            |channel| format!("{channel}/{provider_user_id}"),
        );
        let mut identity = AdapterIdentity::new(
            self.definition.name.clone(),
            self.platform.kind().to_owned(),
            source_id,
        )?;
        if let Some(display_name) = display_name {
            identity = identity.with_display_name(display_name);
        }
        let mut mapped_metadata = Metadata::new();
        mapped_metadata.insert_str("external.platform", self.platform.kind())?;
        if let Some(channel_id) = provider_channel_id {
            mapped_metadata.insert_str("external.channel_id", channel_id)?;
        }
        mapped_metadata.merge(metadata);
        identity.metadata.merge(mapped_metadata);
        Ok(identity)
    }

    fn deliver_update(&mut self, update: AdapterOutboundUpdate) -> Result<()> {
        self.validate_same_adapter(&update.adapter_name, "deliver an update")?;
        Err(self.unsupported_error("outbound delivery"))
    }

    fn acknowledge(&mut self, acknowledgement: AdapterAcknowledgement) -> Result<()> {
        self.validate_same_adapter(&acknowledgement.adapter_name, "acknowledge a message")?;
        Err(self.unsupported_error("acknowledgement delivery"))
    }
}

/// Registers factories for every external chat stub enabled by Cargo features.
///
/// With default features this function is a no-op. Enable one or more
/// `adapter-*` features, or the `external-chat-stubs` umbrella feature, to add
/// placeholder factories to the registry without pulling platform SDKs into the
/// build.
pub fn register_external_chat_adapter_stubs(registry: &mut AdapterRegistry) -> Result<()> {
    for platform in enabled_external_chat_stub_platforms() {
        registry.register_fn(platform.kind(), move |definition| {
            Ok(Box::new(ExternalChatStubAdapter::new_for_platform(
                definition, platform,
            )?))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        AdapterAcknowledgementStatus, AdapterEvent, AdapterEventSink, AdapterOutboundUpdateKind,
        AdapterSession,
    };
    use missive_core::{ErrorCategory, EventId, MessageId};
    use serde_json::json;

    #[derive(Debug)]
    struct NoopSink;

    impl AdapterEventSink for NoopSink {
        fn emit(&self, _event: AdapterEvent) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn platform_info_documents_non_empty_secret_references_permissions_and_behaviors() {
        for platform in ExternalChatPlatform::all() {
            let info = platform.info();
            assert_eq!(ExternalChatPlatform::from_kind(info.kind), Some(platform));
            assert_eq!(info.kind, platform.kind());
            assert_eq!(info.cargo_feature, platform.cargo_feature());
            assert!(!info.required_secret_refs.is_empty());
            assert!(!info.required_permissions.is_empty());
            assert!(!info.platform_behaviors.is_empty());
            for secret_ref in info.required_secret_refs {
                assert!(
                    secret_ref.ends_with("auth_ref")
                        || secret_ref.ends_with("public_key")
                        || secret_ref == &"homeserver_url"
                );
                assert!(!secret_ref.contains("xox"));
                assert!(!secret_ref.contains("token_value"));
            }
        }
    }

    #[test]
    fn register_external_chat_stubs_matches_enabled_features() -> Result<()> {
        let mut registry = AdapterRegistry::new();
        register_external_chat_adapter_stubs(&mut registry)?;

        let mut expected = enabled_external_chat_stub_platforms()
            .into_iter()
            .map(ExternalChatPlatform::kind)
            .collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(registry.kinds(), expected);
        Ok(())
    }

    #[cfg(feature = "external-chat-stubs")]
    #[test]
    fn aggregate_feature_enables_all_external_stub_factories() -> Result<()> {
        let mut registry = AdapterRegistry::new();
        register_external_chat_adapter_stubs(&mut registry)?;

        assert_eq!(
            registry.kinds(),
            vec![
                DISCORD_ADAPTER_KIND,
                EMAIL_ADAPTER_KIND,
                MATRIX_ADAPTER_KIND,
                SLACK_ADAPTER_KIND,
                TELEGRAM_ADAPTER_KIND,
            ]
        );
        Ok(())
    }

    #[test]
    fn stub_maps_identity_but_rejects_live_runtime_operations() -> Result<()> {
        let definition = AdapterDefinition::new("team-slack", SLACK_ADAPTER_KIND)?;
        let mut adapter = ExternalChatStubAdapter::new(definition.clone())?;
        assert_eq!(adapter.platform(), ExternalChatPlatform::Slack);

        let external = AdapterExternalIdentity::new("U123")?
            .with_channel_id("C123")?
            .with_display_name("Ada");
        let identity = adapter.map_identity(external)?;
        assert_eq!(identity.adapter_name, "team-slack");
        assert_eq!(identity.source_kind, SLACK_ADAPTER_KIND);
        assert_eq!(identity.source_id, "C123/U123");
        assert_eq!(identity.display_name.as_deref(), Some("Ada"));
        assert_eq!(
            identity.metadata.get_str("external.platform"),
            Some(SLACK_ADAPTER_KIND)
        );
        assert_eq!(
            identity.metadata.get_str("external.channel_id"),
            Some("C123")
        );

        let start_error = adapter
            .start(AdapterContext::new(definition.clone(), Arc::new(NoopSink)))
            .expect_err("stub must not start as a live adapter");
        assert_eq!(start_error.category(), ErrorCategory::Config);
        assert!(start_error.to_string().contains("feature-gated stub"));

        let update = AdapterOutboundUpdate::new(
            "team-slack",
            EventId::new("evt/external-chat/update-1")?,
            identity,
            AdapterSession::new("default")?,
            AdapterOutboundUpdateKind::Status,
            json!({"state": "queued"}),
        )?;
        let delivery_error = adapter
            .deliver_update(update)
            .expect_err("stub must not deliver updates");
        assert_eq!(delivery_error.category(), ErrorCategory::Config);

        let ack_error = adapter
            .acknowledge(AdapterAcknowledgement::new(
                "team-slack",
                MessageId::new("msg-external-chat-1")?,
                AdapterAcknowledgementStatus::Accepted,
            )?)
            .expect_err("stub must not acknowledge messages on-platform");
        assert_eq!(ack_error.category(), ErrorCategory::Config);

        adapter.stop()?;
        Ok(())
    }

    #[test]
    fn stub_rejects_unknown_and_mismatched_kinds() {
        let unknown = ExternalChatStubAdapter::new(
            AdapterDefinition::new("custom", "custom-chat").expect("definition"),
        )
        .expect_err("unknown kind should fail");
        assert_eq!(unknown.category(), ErrorCategory::Config);

        let mismatched = ExternalChatStubAdapter::new_for_platform(
            AdapterDefinition::new("discord", DISCORD_ADAPTER_KIND).expect("definition"),
            ExternalChatPlatform::Email,
        )
        .expect_err("mismatched kind should fail");
        assert_eq!(mismatched.category(), ErrorCategory::Config);
    }
}
