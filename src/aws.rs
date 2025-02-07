use std::sync::Arc;

use aws_config::{retry::RetryConfig, Region};
use aws_sdk_cloudwatchlogs as cloudwatchlogs;
use aws_smithy_runtime::client::http::hyper_014::HyperClientBuilder;
use hyper_rustls::{ConfigBuilderExt, HttpsConnectorBuilder};
use rustls::{ClientConfig, KeyLogFile};

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
        let mut builder = aws_config::from_env().retry_config(self.retry_config.clone());
        if let Some(profile_name) = &self.profile_name {
            builder = builder.profile_name(profile_name);
        }

        if let Some(region) = &self.region {
            builder = builder.region(Region::new(region.clone()));
        }

        let mut config = ClientConfig::builder()
            .with_safe_defaults()
            .with_native_roots()
            .with_no_client_auth();

        config.key_log = Arc::new(KeyLogFile::new());

        let connector = HttpsConnectorBuilder::new()
            .with_tls_config(config)
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        let hyper_client = HyperClientBuilder::new().build(connector);
        let config = builder.http_client(hyper_client).load().await;

        let client = cloudwatchlogs::Client::new(&config);
        Ok(client)
    }
}
