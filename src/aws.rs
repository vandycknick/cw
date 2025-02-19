use std::sync::Arc;

use aws_config::{retry::RetryConfig, Region};
use aws_sdk_cloudwatchlogs as cloudwatchlogs;
use aws_smithy_runtime::client::http::hyper_014::HyperClientBuilder;
use hyper::client::HttpConnector;
use hyper_rustls::ConfigBuilderExt;
use hyper_rustls::HttpsConnector;
use hyper_rustls::HttpsConnectorBuilder;
use rustls::{ClientConfig, KeyLogFile};

use crate::proxy::{Intercept, Proxy, ProxyConnector};

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

    fn create_https_connector(&self, config: &ClientConfig) -> HttpsConnector<HttpConnector> {
        HttpsConnectorBuilder::new()
            .with_tls_config(config.clone())
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build()
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

        let http_client = if let Ok(https_proxy) = std::env::var("HTTPS_PROXY") {
            if let Ok(proxy_uri) = https_proxy.parse::<hyper::Uri>() {
                log::info!(target: "cw", "HTTPS_PROXY env var is set to {}, configuring to proxy all request to this server.", proxy_uri);
                let proxy = Proxy::new(Intercept::All, proxy_uri);

                let aws_ca_bundle = std::env::var("AWS_CA_BUNDLE");

                let connector = if let Ok(bundle) = aws_ca_bundle {
                    let certs = load_certificates_from_pem(&bundle)?;
                    ProxyConnector::from_proxy_with_self_signed_certificate(
                        self.create_https_connector(&config),
                        proxy,
                        certs.first().unwrap().clone(),
                    )?
                } else {
                    ProxyConnector::from_proxy(self.create_https_connector(&config), proxy)?
                };
                HyperClientBuilder::new().build(connector)
            } else {
                log::error!(target: "cw", "HTTPS_PROXY env var is set to {}, this value isn't a valid Uri. Skipping proxy setup.", https_proxy);
                let connector = self.create_https_connector(&config);
                HyperClientBuilder::new().build(connector)
            }
        } else {
            let connector = self.create_https_connector(&config);
            HyperClientBuilder::new().build(connector)
        };

        let config = builder.http_client(http_client).load().await;

        let client = cloudwatchlogs::Client::new(&config);
        Ok(client)
    }
}

fn load_certificates_from_pem(path: &str) -> std::io::Result<Vec<rustls::Certificate>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let certs = rustls_pemfile::certs(&mut reader);

    let certs = certs
        .into_iter()
        .filter_map(|res| res.ok().map(|v| rustls::Certificate(v.to_vec())))
        .collect();

    Ok(certs)
}
