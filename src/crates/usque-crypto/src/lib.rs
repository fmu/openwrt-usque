mod cert;
mod keys;
mod quiche;
mod tls;

pub use cert::generate_self_signed_cert;
pub use keys::{
    decode_endpoint_public_key, decode_private_key, encode_private_key, generate_ec_keypair,
    private_key_pkcs8_der, public_key_spki_der, PeerPublicKey, SigningKey,
};
pub use quiche::{prepare_quiche_client_credentials, QuicheClientCredentials};
pub use tls::{build_rustls_config, init, TlsOptions};
