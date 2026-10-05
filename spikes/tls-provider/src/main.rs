//! Throwaway (P0.10): installs the aws-lc-rs provider and builds a TLS client
//! with every crate that uses rustls, plus a certificate with rcgen. Nothing
//! connects anywhere. Run it with `cargo run`.

use std::error::Error;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| "a rustls crypto provider was already installed")?;

    let rustls_config = rustls::ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    println!("rustls: client config built");

    let _http = reqwest::Client::builder().build()?;
    println!("reqwest: client built");

    let _grpc = tonic::transport::Endpoint::from_static("https://example.com")
        .tls_config(tonic::transport::ClientTlsConfig::new())?
        .connect_lazy();
    println!("tonic: TLS channel built");

    let _ws = tokio_tungstenite::Connector::Rustls(Arc::new(rustls_config));
    println!("tokio-tungstenite: rustls connector built");

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    println!("rcgen: self-signed certificate, {} DER bytes", cert.cert.der().len());

    Ok(())
}
