use std::fs;

use aws_config::{retry::RetryConfig, Region};
use aws_config::{AppName, BehaviorVersion};
use aws_sdk_cloudwatchlogs as cloudwatchlogs;
use aws_smithy_http_client::proxy::ProxyConfig;
use aws_smithy_http_client::tls::{self, TlsContext, TrustStore};
use aws_smithy_http_client::{Builder, ConnectorBuilder};
use eyre::Context;

pub struct LogClientBuilder {
    profile_name: Option<String>,
    region: Option<String>,
    retry_config: RetryConfig,
}

impl LogClientBuilder {
    pub fn new() -> Self {
        LogClientBuilder {
            profile_name: None,
            region: None,
            retry_config: RetryConfig::standard(),
        }
    }

    pub fn use_profile_name(mut self, profile_name: Option<String>) -> Self {
        self.profile_name = profile_name;
        self
    }

    pub fn use_region(mut self, region: Option<String>) -> Self {
        self.region = region;
        self
    }

    pub async fn build(&self) -> eyre::Result<cloudwatchlogs::Client> {
        let mut config_builder = aws_config::from_env()
            .retry_config(self.retry_config.clone())
            .behavior_version(BehaviorVersion::latest());

        if let Some(profile_name) = &self.profile_name {
            config_builder = config_builder.profile_name(profile_name);
        }

        if let Some(region) = &self.region {
            config_builder = config_builder.region(Region::new(region.clone()));
        }

        let mut store = TrustStore::empty().with_native_roots(true);
        if let Some(cert_bytes) = std::env::var("AWS_CA_BUNDLE")
            .ok()
            .map(|a| fs::read(&a).context(format!("Failed reading AWS_CA_BUNDLE: {}", &a)))
            .transpose()?
        {
            store = store.with_pem_certificate(cert_bytes);
        }
        let context = TlsContext::builder().with_trust_store(store).build()?;

        let http_client =
            Builder::new().build_with_connector_fn(move |settings, runtime_components| {
                let mut conn_builder = ConnectorBuilder::default()
                    .tls_provider(tls::Provider::Rustls(
                        tls::rustls_provider::CryptoMode::AwsLc,
                    ))
                    .tls_context(context.clone());

                conn_builder.set_connector_settings(settings.cloned());
                if let Some(components) = runtime_components {
                    conn_builder.set_sleep_impl(components.sleep_impl());
                }

                conn_builder.set_proxy_config(Some(ProxyConfig::from_env()));
                conn_builder.build()
            });

        let config = config_builder
            .app_name(AppName::new("cw").unwrap())
            .http_client(http_client)
            .load()
            .await;

        let client = cloudwatchlogs::Client::new(&config);
        Ok(client)
    }
}
