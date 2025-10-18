use crate::http::client::Builder;
use aws_config::{retry::RetryConfig, Region};
use aws_config::{AppName, BehaviorVersion};
use aws_sdk_cloudwatchlogs as cloudwatchlogs;

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

        let http_client = Builder::new()
            .with_proxy(
                std::env::var("HTTPS_PROXY")
                    .ok()
                    .and_then(|u| {
                        u.parse::<hyper::Uri>()
                          .inspect_err(|e| tracing::error!(target: "cw", "Failed parsing parsing HTTPS_PROXY url: {}", e))
                          .ok()
                    })
            )
            .with_custom_certs(std::env::var("AWS_CA_BUNDLE").ok().and_then(|a| {
                load_certificates_from_pem(&a)
                    .inspect_err(|e| tracing::error!(target: "cw", "Failed to load certificates from AWS_CA_BUNDLE with error: {}", e))
                    .ok()
            }))
            .build_https();

        let config = config_builder
            .app_name(AppName::new("cw").unwrap())
            .http_client(http_client)
            .load()
            .await;

        let client = cloudwatchlogs::Client::new(&config);
        Ok(client)
    }
}

fn load_certificates_from_pem(
    path: &str,
) -> eyre::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);

    let certs = rustls_pemfile::certs(&mut reader)
        .map(|c| c.map_err(Into::into))
        .into_iter()
        .collect::<eyre::Result<Vec<_>>>()?;

    if certs.is_empty() {
        Err(eyre::eyre!("No certificates found in file {}", path))
    } else {
        Ok(certs)
    }
}
