use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::client::conn::http1 as client_http1;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Uri;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tracing::info;

use usque_virtual_net::dns::DnsResolver;
use usque_virtual_net::VirtualStack;

pub struct HttpProxyConfig {
    pub bind: SocketAddr,
    pub username: Option<String>,
    pub password: Option<String>,
    pub resolver: DnsResolver,
}

pub async fn run(cfg: HttpProxyConfig, stack: Arc<VirtualStack>) -> Result<()> {
    let auth = match (&cfg.username, &cfg.password) {
        (Some(u), Some(p)) => Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            format!("{u}:{p}"),
        )),
        _ => None,
    };

    let listener = TcpListener::bind(cfg.bind).await?;
    info!("HTTP proxy listening on {}", cfg.bind);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let auth = auth.clone();
        let resolver = cfg.resolver.clone();
        let stack = Arc::clone(&stack);

        tokio::spawn(async move {
            let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                let auth = auth.clone();
                let resolver = resolver.clone();
                let stack = Arc::clone(&stack);
                async move { handle(req, auth, resolver, stack).await }
            });

            if let Err(err) = http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                tracing::debug!("http proxy connection error: {err}");
            }
        });
    }
}

async fn handle(
    req: Request<hyper::body::Incoming>,
    auth: Option<String>,
    resolver: DnsResolver,
    stack: Arc<VirtualStack>,
) -> Result<Response<Full<bytes::Bytes>>, hyper::Error> {
    if let Some(expected) = auth {
        let provided = req
            .headers()
            .get("proxy-authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != format!("Basic {expected}") {
            return Ok(Response::builder()
                .status(StatusCode::PROXY_AUTHENTICATION_REQUIRED)
                .header("Proxy-Authenticate", "Basic realm=\"Proxy\"")
                .body(Full::new(bytes::Bytes::new()))
                .unwrap());
        }
    }

    if req.method() == Method::CONNECT {
        return handle_connect(req, resolver, stack).await;
    }

    handle_http(req, resolver, stack).await
}

async fn handle_connect(
    req: Request<hyper::body::Incoming>,
    resolver: DnsResolver,
    stack: Arc<VirtualStack>,
) -> Result<Response<Full<bytes::Bytes>>, hyper::Error> {
    let authority = req
        .uri()
        .authority()
        .map(|a| a.to_string())
        .unwrap_or_default();

    let addr = resolve_connect_addr(&authority, &resolver).await;

    let mut dest = match stack.dial_tcp(addr).await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::debug!("connect dial failed: {err}");
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(bytes::Bytes::new()))
                .unwrap());
        }
    };

    tokio::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(upgraded) => {
                let mut upgraded = TokioIo::new(upgraded);
                let _ = tokio::io::copy_bidirectional(&mut upgraded, &mut dest).await;
            }
            Err(err) => tracing::debug!("upgrade failed: {err}"),
        }
    });

    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Full::new(bytes::Bytes::new()))
        .unwrap())
}

async fn resolve_connect_addr(authority: &str, resolver: &DnsResolver) -> SocketAddr {
    if let Ok(addr) = authority.parse::<SocketAddr>() {
        return addr;
    }

    if let Some((host, port)) = authority.rsplit_once(':') {
        if let Ok(port) = port.parse::<u16>() {
            if let Ok(ip) = resolver.lookup_ip(host).await {
                return SocketAddr::new(ip, port);
            }
        }
    }

    SocketAddr::from(([127, 0, 0, 1], 443))
}

async fn handle_http(
    req: Request<hyper::body::Incoming>,
    resolver: DnsResolver,
    stack: Arc<VirtualStack>,
) -> Result<Response<Full<bytes::Bytes>>, hyper::Error> {
    let (parts, body) = req.into_parts();
    let uri = parts.uri.clone();
    let host = uri.host().unwrap_or("localhost");
    let port = uri.port_u16().unwrap_or(80);
    let ip = resolver
        .lookup_ip(host)
        .await
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let addr = SocketAddr::new(ip, port);

    let stream = match stack.dial_tcp(addr).await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::debug!("http dial failed: {err}");
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(bytes::Bytes::new()))
                .unwrap());
        }
    };

    let io = TokioIo::new(stream);
    let (mut sender, connection) = match client_http1::handshake(io).await {
        Ok(parts) => parts,
        Err(err) => {
            tracing::debug!("http client handshake failed: {err}");
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(bytes::Bytes::new()))
                .unwrap());
        }
    };

    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::debug!("http upstream connection error: {err}");
        }
    });

    let body = body.collect().await?.to_bytes();
    let origin_uri = origin_form_uri(&uri);
    let mut builder = Request::builder()
        .method(parts.method)
        .uri(origin_uri)
        .version(parts.version);

    for (name, value) in &parts.headers {
        builder = builder.header(name, value);
    }

    if !parts.headers.contains_key(hyper::header::HOST) {
        builder = builder.header(hyper::header::HOST, authority_header_value(&uri));
    }

    let upstream_req = match builder.body(Full::new(body)) {
        Ok(req) => req,
        Err(err) => {
            tracing::debug!("failed to build upstream request: {err}");
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(bytes::Bytes::new()))
                .unwrap());
        }
    };

    let upstream_res = match sender.send_request(upstream_req).await {
        Ok(res) => res,
        Err(err) => {
            tracing::debug!("upstream request failed: {err}");
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(bytes::Bytes::new()))
                .unwrap());
        }
    };

    let (parts, body) = upstream_res.into_parts();
    let body = body.collect().await?.to_bytes();
    let mut response = Response::builder()
        .status(parts.status)
        .version(parts.version);
    for (name, value) in &parts.headers {
        response = response.header(name, value);
    }

    Ok(response.body(Full::new(body)).unwrap())
}

fn origin_form_uri(uri: &Uri) -> Uri {
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    path_and_query
        .parse()
        .unwrap_or_else(|_| Uri::from_static("/"))
}

fn authority_header_value(uri: &Uri) -> String {
    match (uri.host(), uri.port_u16()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        (None, _) => "localhost".to_string(),
    }
}
