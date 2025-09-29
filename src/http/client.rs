use crate::http::timeout;
use crate::proxy::{Intercept, Proxy, ProxyConnector};
use aws_smithy_runtime_api::client::dns::ResolveDns;

use aws_smithy_async::future::timeout::TimedOutError;
use aws_smithy_async::rt::sleep::{default_async_sleep, AsyncSleep, SharedAsyncSleep};
use aws_smithy_runtime_api::box_error::BoxError;
use aws_smithy_runtime_api::client::connection::CaptureSmithyConnection;
use aws_smithy_runtime_api::client::connection::ConnectionMetadata;
use aws_smithy_runtime_api::client::connector_metadata::ConnectorMetadata;
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpClient,
    SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::client::runtime_components::{
    RuntimeComponents, RuntimeComponentsBuilder,
};
use aws_smithy_runtime_api::shared::IntoShared;
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::config_bag::ConfigBag;
use aws_smithy_types::retry::ErrorKind;
use h2::Reason;
use http::{Extensions, Uri};
use hyper::rt::{Read, Write};
use hyper_util::client::legacy::connect::dns::GaiResolver;
use hyper_util::client::legacy::connect::{
    capture_connection, CaptureConnection, Connect, Connection,
    HttpConnector as HyperHttpConnector, HttpInfo,
};
use hyper_util::rt::TokioExecutor;
use rustls::KeyLogFile;
use rustls_pki_types::CertificateDer;
use std::borrow::Cow;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tower::Service;

#[derive(Clone, Default, Debug)]
pub struct Builder {
    client_builder: Option<hyper_util::client::legacy::Builder>,
    proxy: Option<crate::proxy::Proxy>,
    certs: Option<Vec<CertificateDer<'static>>>,
}

impl Builder {
    /// Creates a new builder.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_proxy(self, uri: Option<hyper::Uri>) -> Self {
        Self {
            client_builder: self.client_builder,
            proxy: uri.map(|u| Proxy::new(Intercept::All, u)),
            certs: self.certs,
        }
    }

    pub fn with_custom_certs(self, certs: Option<Vec<CertificateDer<'static>>>) -> Self {
        Self {
            client_builder: self.client_builder,
            proxy: self.proxy,
            certs: certs,
        }
    }

    pub fn build_http(self) -> SharedHttpClient {
        build_with_conn_fn(
            self.client_builder,
            move |client_builder, settings, runtime_components| {
                let builder = new_conn_builder(
                    client_builder,
                    settings,
                    runtime_components,
                    self.proxy.clone(),
                    self.certs.clone(),
                );
                builder.build_http()
            },
        )
    }

    /// Create an HTTPS client with the selected TLS provider.
    ///
    /// The trusted certificates will be loaded later when this becomes the selected
    /// HTTP client for a Smithy client.
    pub fn build_https(self) -> SharedHttpClient {
        build_with_conn_fn(
            self.client_builder,
            move |client_builder, settings, runtime_components| {
                let builder = new_conn_builder(
                    client_builder,
                    settings,
                    runtime_components,
                    self.proxy.clone(),
                    self.certs.clone(),
                );
                builder.build()
            },
        )
    }

    /// Create an HTTPS client using a custom DNS resolver
    pub fn build_with_resolver(
        self,
        resolver: impl ResolveDns + Clone + 'static,
    ) -> SharedHttpClient {
        build_with_conn_fn(
            self.client_builder,
            move |client_builder, settings, runtime_components| {
                let builder = new_conn_builder(
                    client_builder,
                    settings,
                    runtime_components,
                    self.proxy.clone(),
                    self.certs.clone(),
                );
                builder.build_with_resolver(resolver.clone())
            },
        )
    }
}

/// [`HttpConnector`] used to make HTTP requests.
///
/// This connector also implements socket connect and read timeouts.
///
/// This shouldn't be used directly in most cases.
/// See the docs on [`Builder`] for examples of how to customize the HTTP client.
#[derive(Debug)]
pub struct Connector {
    adapter: Box<dyn HttpConnector>,
}

impl Connector {
    /// Builder for an HTTP connector.
    pub fn builder() -> ConnectorBuilder {
        ConnectorBuilder {
            enable_tcp_nodelay: true,
            ..Default::default()
        }
    }
}

impl HttpConnector for Connector {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        self.adapter.call(request)
    }
}

/// Builder for [`Connector`].
#[derive(Default, Debug)]
pub struct ConnectorBuilder {
    connector_settings: Option<HttpConnectorSettings>,
    sleep_impl: Option<SharedAsyncSleep>,
    client_builder: Option<hyper_util::client::legacy::Builder>,
    enable_tcp_nodelay: bool,
    interface: Option<String>,
    proxy: Option<crate::proxy::Proxy>,
    certs: Option<Vec<CertificateDer<'static>>>,
}

impl ConnectorBuilder {
    /// Build an HTTP connector without TLS
    pub fn build_http(self) -> Connector {
        let base = self.base_connector();
        self.wrap_connector(base)
    }

    /// Build a [`Connector`] that will use the default DNS resolver implementation.
    pub fn build(self) -> Connector {
        let http_connector = self.base_connector();
        self.build_https(http_connector)
    }

    /// Build a [`Connector`] that will use the given DNS resolver implementation.
    pub fn build_with_resolver<R: ResolveDns + Clone + 'static>(self, resolver: R) -> Connector {
        use crate::http::dns::HyperUtilResolver;
        let http_connector = self.base_connector_with_resolver(HyperUtilResolver { resolver });
        self.build_https(http_connector)
    }

    fn build_https<R>(self, mut http_connector: HyperHttpConnector<R>) -> Connector
    where
        R: Clone + Send + Sync + 'static,
        R: tower::Service<hyper_util::client::legacy::connect::dns::Name>,
        R::Response: Iterator<Item = std::net::SocketAddr>,
        R::Future: Send,
        R::Error: Into<Box<dyn Error + Send + Sync>>,
    {
        // let root_certs = tls_context.rustls_root_certs();
        let mut roots = tokio_rustls::rustls::RootCertStore::empty();
        let root_certs = rustls_native_certs::load_native_certs();
        roots.add_parsable_certificates(root_certs.certs);

        if let Some(ref certs) = self.certs {
            roots.add_parsable_certificates(certs.clone());
        }

        http_connector.enforce_http(false);

        let mut tls_config = rustls::ClientConfig::builder_with_provider(
            rustls::crypto::aws_lc_rs::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .expect("Error with the TLS configuration.")
        .with_root_certificates(roots)
        // .with_native_roots()
        // .expect("Error with the TLS configuration.")
        .with_no_client_auth();

        tls_config.key_log = Arc::new(KeyLogFile::new());

        let wrapped = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(http_connector);

        self.wrap_connector(wrapped)
    }

    /// Create a [`Connector`] from this builder and a given connector.
    pub(crate) fn wrap_connector<C>(self, tcp_connector: C) -> Connector
    where
        C: Send + Sync + 'static,
        C: Clone,
        C: tower::Service<Uri>,
        C::Response: Read + Write + Connection + Send + Sync + Unpin,
        C: Connect,
        C::Future: Unpin + Send + 'static,
        C::Error: Into<BoxError>,
    {
        let client_builder =
            self.client_builder
                .unwrap_or(hyper_util::client::legacy::Builder::new(
                    TokioExecutor::new(),
                ));
        let sleep_impl = self.sleep_impl.or_else(default_async_sleep);
        let (connect_timeout, read_timeout) = self
            .connector_settings
            .map(|c| (c.connect_timeout(), c.read_timeout()))
            .unwrap_or((None, None));

        let proxied = if let Some(proxy) = self.proxy {
            ProxyConnector::from_proxy(tcp_connector, proxy, self.certs)
        } else {
            ProxyConnector::new(tcp_connector, None)
        };

        let connector = match connect_timeout {
            Some(duration) => timeout::ConnectTimeout::new(
                proxied,
                sleep_impl
                    .clone()
                    .expect("a sleep impl must be provided in order to have a connect timeout"),
                duration,
            ),
            None => timeout::ConnectTimeout::no_timeout(proxied),
        };

        let base = client_builder.build(connector);

        let read_timeout = match read_timeout {
            Some(duration) => timeout::HttpReadTimeout::new(
                base,
                sleep_impl.expect("a sleep impl must be provided in order to have a read timeout"),
                duration,
            ),
            None => timeout::HttpReadTimeout::no_timeout(base),
        };
        Connector {
            adapter: Box::new(Adapter {
                client: read_timeout,
            }),
        }
    }

    /// Get the base TCP connector by mapping our config to the underlying `HttpConnector` from hyper
    /// (which is a base TCP connector with no TLS or any wrapping)
    fn base_connector(&self) -> HyperHttpConnector {
        self.base_connector_with_resolver(GaiResolver::new())
    }

    /// Get the base TCP connector by mapping our config to the underlying `HttpConnector` from hyper
    /// using the given resolver `R`
    fn base_connector_with_resolver<R>(&self, resolver: R) -> HyperHttpConnector<R> {
        let mut conn = HyperHttpConnector::new_with_resolver(resolver);
        conn.set_nodelay(self.enable_tcp_nodelay);
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        if let Some(interface) = &self.interface {
            conn.set_interface(interface);
        }
        conn
    }

    pub fn with_proxy(mut self, proxy: Option<crate::proxy::Proxy>) -> Self {
        self.proxy = proxy;
        self
    }

    pub fn with_certificates(mut self, certs: Option<Vec<CertificateDer<'static>>>) -> Self {
        self.certs = certs;
        self
    }

    /// Set the async sleep implementation used for timeouts
    ///
    /// Calling this is only necessary for testing or to use something other than
    /// [`default_async_sleep`].
    pub fn sleep_impl(mut self, sleep_impl: impl AsyncSleep + 'static) -> Self {
        self.sleep_impl = Some(sleep_impl.into_shared());
        self
    }

    /// Set the async sleep implementation used for timeouts
    ///
    /// Calling this is only necessary for testing or to use something other than
    /// [`default_async_sleep`].
    pub fn set_sleep_impl(&mut self, sleep_impl: Option<SharedAsyncSleep>) -> &mut Self {
        self.sleep_impl = sleep_impl;
        self
    }

    /// Configure the HTTP settings for the `HyperAdapter`
    pub fn connector_settings(mut self, connector_settings: HttpConnectorSettings) -> Self {
        self.connector_settings = Some(connector_settings);
        self
    }

    /// Configure the HTTP settings for the `HyperAdapter`
    pub fn set_connector_settings(
        &mut self,
        connector_settings: Option<HttpConnectorSettings>,
    ) -> &mut Self {
        self.connector_settings = connector_settings;
        self
    }

    /// Configure `SO_NODELAY` for all sockets to the supplied value `nodelay`
    pub fn enable_tcp_nodelay(mut self, nodelay: bool) -> Self {
        self.enable_tcp_nodelay = nodelay;
        self
    }

    /// Configure `SO_NODELAY` for all sockets to the supplied value `nodelay`
    pub fn set_enable_tcp_nodelay(&mut self, nodelay: bool) -> &mut Self {
        self.enable_tcp_nodelay = nodelay;
        self
    }

    /// Sets the value for the `SO_BINDTODEVICE` option on this socket.
    ///
    /// If a socket is bound to an interface, only packets received from that particular
    /// interface are processed by the socket. Note that this only works for some socket
    /// types (e.g. `AF_INET` sockets).
    ///
    /// On Linux it can be used to specify a [VRF], but the binary needs to either have
    /// `CAP_NET_RAW` capability set or be run as root.
    ///
    /// This function is only available on Android, Fuchsia, and Linux.
    ///
    /// [VRF]: https://www.kernel.org/doc/Documentation/networking/vrf.txt
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    pub fn set_interface<S: Into<String>>(&mut self, interface: S) -> &mut Self {
        self.interface = Some(interface.into());
        self
    }

    /// Override the Hyper client [`Builder`](hyper_util::client::legacy::Builder) used to construct this client.
    ///
    /// This enables changing settings like forcing HTTP2 and modifying other default client behavior.
    pub(crate) fn hyper_builder(
        mut self,
        hyper_builder: hyper_util::client::legacy::Builder,
    ) -> Self {
        self.set_hyper_builder(Some(hyper_builder));
        self
    }

    /// Override the Hyper client [`Builder`](hyper_util::client::legacy::Builder) used to construct this client.
    ///
    /// This enables changing settings like forcing HTTP2 and modifying other default client behavior.
    pub(crate) fn set_hyper_builder(
        &mut self,
        hyper_builder: Option<hyper_util::client::legacy::Builder>,
    ) -> &mut Self {
        self.client_builder = hyper_builder;
        self
    }
}

/// Adapter to use a Hyper 1.0-based Client as an `HttpConnector`
///
/// This adapter also enables TCP `CONNECT` and HTTP `READ` timeouts via [`Connector::builder`].
struct Adapter<C> {
    client: timeout::HttpReadTimeout<
        hyper_util::client::legacy::Client<timeout::ConnectTimeout<C>, SdkBody>,
    >,
}

impl<C> fmt::Debug for Adapter<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Adapter")
            .field("client", &"** hyper client **")
            .finish()
    }
}

/// Extract a smithy connection from a hyper CaptureConnection
fn extract_smithy_connection(capture_conn: &CaptureConnection) -> Option<ConnectionMetadata> {
    let capture_conn = capture_conn.clone();
    if let Some(conn) = capture_conn.clone().connection_metadata().as_ref() {
        let mut extensions = Extensions::new();
        conn.get_extras(&mut extensions);
        let http_info = extensions.get::<HttpInfo>();
        let mut builder = ConnectionMetadata::builder()
            .proxied(conn.is_proxied())
            .poison_fn(move || match capture_conn.connection_metadata().as_ref() {
                Some(conn) => conn.poison(),
                None => tracing::error!("no connection existed to poison"),
            });

        builder
            .set_local_addr(http_info.map(|info| info.local_addr()))
            .set_remote_addr(http_info.map(|info| info.remote_addr()));

        let smithy_connection = builder.build();

        Some(smithy_connection)
    } else {
        None
    }
}

impl<C> HttpConnector for Adapter<C>
where
    C: Clone + Send + Sync + 'static,
    C: tower::Service<Uri>,
    C::Response: Connection + Read + Write + Unpin + 'static,
    timeout::ConnectTimeout<C>: Connect,
    C::Future: Unpin + Send + 'static,
    C::Error: Into<BoxError>,
{
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let mut request = match request.try_into_http1x() {
            Ok(request) => request,
            Err(err) => {
                return HttpConnectorFuture::ready(Err(ConnectorError::user(err.into())));
            }
        };

        let capture_connection = capture_connection(&mut request);
        if let Some(capture_smithy_connection) =
            request.extensions().get::<CaptureSmithyConnection>()
        {
            capture_smithy_connection
                .set_connection_retriever(move || extract_smithy_connection(&capture_connection));
        }

        let mut client = self.client.clone();
        let fut = client.call(request);
        HttpConnectorFuture::new(async move {
            let response = fut
                .await
                .map_err(downcast_error)?
                .map(SdkBody::from_body_1_x);
            match HttpResponse::try_from(response) {
                Ok(response) => Ok(response),
                Err(err) => Err(ConnectorError::other(err.into(), None)),
            }
        })
    }
}

/// Downcast errors coming out of hyper into an appropriate `ConnectorError`
fn downcast_error(err: BoxError) -> ConnectorError {
    // is a `TimedOutError` (from aws_smithy_async::timeout) in the chain? if it is, this is a timeout
    if find_source::<TimedOutError>(err.as_ref()).is_some() {
        return ConnectorError::timeout(err);
    }
    // is the top of chain error actually already a `ConnectorError`? return that directly
    let err = match err.downcast::<ConnectorError>() {
        Ok(connector_error) => return *connector_error,
        Err(box_error) => box_error,
    };
    // generally, the top of chain will probably be a hyper error. Go through a set of hyper specific
    // error classifications
    let err = match find_source::<hyper::Error>(err.as_ref()) {
        Some(hyper_error) => return to_connector_error(hyper_error)(err),
        None => err,
    };

    // otherwise, we have no idea!
    ConnectorError::other(err, None)
}

/// Convert a [`hyper::Error`] into a [`ConnectorError`]
fn to_connector_error(err: &hyper::Error) -> fn(BoxError) -> ConnectorError {
    if err.is_timeout() || find_source::<timeout::HttpTimeoutError>(err).is_some() {
        return ConnectorError::timeout;
    }
    if err.is_user() {
        return ConnectorError::user;
    }
    if err.is_closed() || err.is_canceled() || find_source::<std::io::Error>(err).is_some() {
        return ConnectorError::io;
    }
    // We sometimes receive this from S3: hyper::Error(IncompleteMessage)
    if err.is_incomplete_message() {
        return |err: BoxError| ConnectorError::other(err, Some(ErrorKind::TransientError));
    }

    if let Some(h2_err) = find_source::<h2::Error>(err) {
        if h2_err.is_go_away()
            || (h2_err.is_reset() && h2_err.reason() == Some(Reason::REFUSED_STREAM))
        {
            return ConnectorError::io;
        }
    }

    // tracing::warn!(err = %DisplayErrorContext(&err), "unrecognized error from Hyper. If this error should be retried, please file an issue.");
    |err: BoxError| ConnectorError::other(err, None)
}

fn find_source<'a, E: Error + 'static>(err: &'a (dyn Error + 'static)) -> Option<&'a E> {
    let mut next = Some(err);
    while let Some(err) = next {
        if let Some(matching_err) = err.downcast_ref::<E>() {
            return Some(matching_err);
        }
        next = err.source();
    }
    None
}

// TODO(https://github.com/awslabs/aws-sdk-rust/issues/1090): CacheKey must also include ptr equality to any
// runtime components that are used—sleep_impl as a base (unless we prohibit overriding sleep impl)
// If we decide to put a DnsResolver in RuntimeComponents, then we'll need to handle that as well.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct CacheKey {
    connect_timeout: Option<Duration>,
    read_timeout: Option<Duration>,
}

impl From<&HttpConnectorSettings> for CacheKey {
    fn from(value: &HttpConnectorSettings) -> Self {
        Self {
            connect_timeout: value.connect_timeout(),
            read_timeout: value.read_timeout(),
        }
    }
}

struct HyperClient<F> {
    connector_cache: RwLock<HashMap<CacheKey, SharedHttpConnector>>,
    client_builder: hyper_util::client::legacy::Builder,
    connector_fn: F,
}

impl<F> fmt::Debug for HyperClient<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HyperClient")
            .field("connector_cache", &self.connector_cache)
            .field("client_builder", &self.client_builder)
            .finish()
    }
}

impl<F> HttpClient for HyperClient<F>
where
    F: Fn(
            hyper_util::client::legacy::Builder,
            Option<&HttpConnectorSettings>,
            Option<&RuntimeComponents>,
        ) -> Connector
        + Send
        + Sync
        + 'static,
{
    fn http_connector(
        &self,
        settings: &HttpConnectorSettings,
        components: &RuntimeComponents,
    ) -> SharedHttpConnector {
        let key = CacheKey::from(settings);
        let mut connector = self.connector_cache.read().unwrap().get(&key).cloned();
        if connector.is_none() {
            let mut cache = self.connector_cache.write().unwrap();
            // Short-circuit if another thread already wrote a connector to the cache for this key
            if !cache.contains_key(&key) {
                let start = components.time_source().map(|ts| ts.now());
                let connector = (self.connector_fn)(
                    self.client_builder.clone(),
                    Some(settings),
                    Some(components),
                );
                let end = components.time_source().map(|ts| ts.now());
                if let (Some(start), Some(end)) = (start, end) {
                    if let Ok(elapsed) = end.duration_since(start) {
                        tracing::debug!("new connector created in {:?}", elapsed);
                    }
                }
                let connector = SharedHttpConnector::new(connector);
                cache.insert(key.clone(), connector);
            }
            connector = cache.get(&key).cloned();
        }

        connector.expect("cache populated above")
    }

    fn validate_base_client_config(
        &self,
        _: &RuntimeComponentsBuilder,
        _: &ConfigBag,
    ) -> Result<(), BoxError> {
        // Initialize the TCP connector at this point so that native certs load
        // at client initialization time instead of upon first request. We do it
        // here rather than at construction so that it won't run if this is not
        // the selected HTTP client for the base config (for example, if this was
        // the default HTTP client, and it was overridden by a later plugin).
        let _ = (self.connector_fn)(self.client_builder.clone(), None, None);
        Ok(())
    }

    fn connector_metadata(&self) -> Option<ConnectorMetadata> {
        Some(ConnectorMetadata::new("hyper", Some(Cow::Borrowed("1.x"))))
    }
}

pub(crate) fn build_with_conn_fn<F>(
    client_builder: Option<hyper_util::client::legacy::Builder>,
    connector_fn: F,
) -> SharedHttpClient
where
    F: Fn(
            hyper_util::client::legacy::Builder,
            Option<&HttpConnectorSettings>,
            Option<&RuntimeComponents>,
        ) -> Connector
        + Send
        + Sync
        + 'static,
{
    SharedHttpClient::new(HyperClient {
        connector_cache: RwLock::new(HashMap::new()),
        client_builder: client_builder
            .unwrap_or_else(|| hyper_util::client::legacy::Builder::new(TokioExecutor::new())),
        connector_fn,
    })
}

// #[allow(dead_code)]
// pub(crate) fn build_with_tcp_conn_fn<C, F>(
//     client_builder: Option<hyper_util::client::legacy::Builder>,
//     tcp_connector_fn: F,
// ) -> SharedHttpClient
// where
//     F: Fn() -> C + Send + Sync + 'static,
//     C: Clone + Send + Sync + 'static,
//     C: tower::Service<Uri>,
//     C::Response: Connection + Read + Write + Send + Sync + Unpin + 'static,
//     C::Future: Unpin + Send + 'static,
//     C::Error: Into<BoxError>,
//     C: Connect,
// {
//     build_with_conn_fn(
//         client_builder,
//         move |client_builder, settings, runtime_components| {
//             let builder = new_conn_builder(client_builder, settings, runtime_components);
//             builder.wrap_connector(tcp_connector_fn())
//         },
//     )
// }

fn new_conn_builder(
    client_builder: hyper_util::client::legacy::Builder,
    settings: Option<&HttpConnectorSettings>,
    runtime_components: Option<&RuntimeComponents>,
    proxy: Option<crate::proxy::Proxy>,
    certs: Option<Vec<CertificateDer<'static>>>,
) -> ConnectorBuilder {
    let mut builder = Connector::builder()
        .with_proxy(proxy)
        .with_certificates(certs)
        .hyper_builder(client_builder);
    builder.set_connector_settings(settings.cloned());
    if let Some(components) = runtime_components {
        builder.set_sleep_impl(components.sleep_impl());
    }
    builder
}
