# Source patches applied to the vendored usque-rs

Provenance: `src/` is a vendored copy of
[FireMasterK/usque-rs](https://github.com/FireMasterK/usque-rs) at commit
`f2a073c3cd23d45e792cbb7912d6b1461b1356c2` (MIT, see `src/LICENSE.md`).

Every change below exists to make the client build for OpenWrt 25.12
`arm_cortex-a7_neon-vfpv4` (armv7, musl) as a fully static, stripped binary that
fits a ~3.3 MB overlay. Nothing here changes the MASQUE protocol on the wire.
Original files remain under the same paths in the upstream commit.

Patches are grouped in the order they were applied.

## 1. Crypto: `aws-lc-rs` → `ring` (32-bit ARM support)

`aws-lc-rs` (and BoringSSL) cannot be built for 32-bit ARM, so the whole
workspace now uses `ring` for TLS, QUIC and certificate signing.

| File | Change |
| --- | --- |
| `src/Cargo.toml` | `rustls` feature `ring` (was `aws-lc-rs`), `quinn` `rustls-ring`, `tokio-rustls` `ring`, `rcgen` `ring`; workspace dependency `ring = "0.17"`; `aws-lc-rs`, `boring`, `tokio-quiche` removed |
| `crates/usque-crypto/src/tls.rs` | install `rustls::crypto::ring::default_provider()`; build the `ClientConfig` with `builder_with_provider(ring)`; the `--insecure` verifier now reports the provider's supported schemes instead of a hard-coded list |
| `crates/usque-crypto/src/keys.rs` | rewritten on `ring::signature::EcdsaKeyPair` (`ECDSA_P256_SHA256_ASN1_SIGNING`). See §2 |
| `crates/usque-crypto/src/cert.rs` | unchanged logic; `rcgen` now signs through the `ring` backend with the PKCS#8 v1 key |
| `crates/usque-crypto/src/quiche.rs` | deleted (quiche/BoringSSL client credentials) |
| `crates/usque-cloudflare-api/src/client.rs` | random serial/WireGuard key now from `usque_crypto::fill_random` (`ring`'s system RNG) instead of `rand` |

## 2. Crypto: P-256 key handling on `ring` (`crates/usque-crypto/src/keys.rs`)

`ring` hides the private scalar and rejects PKCS#8 documents whose inner
`ECPrivateKey` lacks the public key, while the Go client stores a *bare* SEC1
`ECPrivateKey` in `config.json`. The key module therefore:

* keeps the canonical **PKCS#8 v1** DER next to the parsed key pair
  (`SigningKey { key, pkcs8 }`), so `encode_private_key` / `private_key_pkcs8_der`
  never need to re-derive it;
* generates keys with `EcdsaKeyPair::generate_pkcs8` (PKCS#8 v1, public key
  embedded, 138 bytes);
* decodes existing configs in both encodings (`decode_private_key`):
  * PKCS#8 v1 directly with `EcdsaKeyPair::from_pkcs8`, and
  * legacy SEC1 `ECPrivateKey` (RFC 5915) by parsing `version`, `privateKey`,
    optional `parameters [0]` and `[1] publicKey` with the in-tree DER reader,
    feeding scalar+point to `EcdsaKeyPair::from_private_key_and_public_key`
    (which verifies they match), then re-wrapping them as PKCS#8 v1;
* builds the 91-byte `SubjectPublicKeyInfo` as the fixed P-256 SPKI prefix
  (`30 59 30 13 … 03 42 00`) plus `ring`'s 65-byte uncompressed point;
* builds PKCS#8 v1 around a 121-byte RFC 5915 `ECPrivateKey`
  (`30 77 …` with `parameters [0]` **after** the scalar and `publicKey [1]`
  last, `OCTET STRING` lengths in DER short form — `ring`'s parser rejects the
  non-minimal long form).

A SEC1 document without an embedded public key is rejected with an actionable
error (`usque enroll --regen-key`), because `ring` cannot derive a public key
from a scalar.

Tests (`crates/usque-crypto/src/keys.rs`, `cert.rs`, `der.rs`) cover: generated
key round-trip, SPKI prefix layout, Go-style 121-byte SEC1 input (synthesised,
never a real key), SEC1 without `parameters`, SEC1 without public key, garbage
input, and that a SEC1-decoded key builds a usable self-signed certificate whose
SPKI matches the key.

## 3. Crypto: in-tree DER reader replaces `x509-parser`

| File | Change |
| --- | --- |
| `crates/usque-crypto/src/der.rs` | new: minimal definite-length DER reader + `certificate_spki_der()`, which walks `Certificate → tbsCertificate → subjectPublicKeyInfo` and returns the SPKI TLV verbatim |
| `crates/usque-crypto/src/tls.rs` | endpoint pinning compares `certificate_spki_der(cert)` with the configured key instead of `x509_parser::parse_x509_certificate`; `x509-parser` dependency dropped |
| `crates/usque-crypto/src/keys.rs`, `cert.rs` | use the shared reader in tests |

Behaviour is identical to the previous `x509-parser` comparison: byte-exact SPKI
match, fail-closed on parse errors, certificate signatures are not verified
(same as upstream — the endpoint is pinned by public key, not by chain).

## 4. Remove SOCKS5 / HTTP proxy / port forwarding / userspace netstack

Only the TUN client is needed on a router; the proxy crates also pulled in
`smoltcp`, `hyper`, `futures` server plumbing and `fast-socks5`.

* deleted `crates/usque-proxy-socks/`, `crates/usque-proxy-http/`,
  `crates/usque-portfw/`, `crates/usque-virtual-net/`
  (`usque-virtual-net`'s `ChannelDevice`/`VirtualStack` were only used by those);
* deleted `crates/usque-cli/src/cmd/{socks,http_proxy,portfw}.rs`;
  `Commands` now contains only `register`, `enroll`, `native-tun`;
* `crates/usque-cli/src/runtime/` collapsed into `src/runtime.rs`: keeps
  `load_config`, `TunnelFlags`, `tunnel_addresses`, `build_connect_options`;
  drops `spawn_userspace_tunnel`;
* dropped workspace dependencies `smoltcp`, `fast-socks5`, `hyper`,
  `hyper-util`, `http-body-util`, plus `rand` (replaced by `ring`'s RNG) and
  `chrono` (see §7);
* deleted `crates/usque-tunnel-core/examples/tunnel_harness.rs` and its
  dev-dependencies (they only exercised the removed virtual net).

`MaintainTunnelConfig::activity` (a proxy-wakeup `Notify`) is now always `None`
but kept to avoid churn in `usque-tunnel-core`; it is dead weight only in the
sense of a single `Option` field.

## 5. Remove quiche/BoringSSL, HTTP/2 transport

* `crates/usque-masque/src/h3.rs` deleted (tokio-quiche/BoringSSL backend);
  feature `quiche` and dependencies `boring`, `tokio-quiche` removed;
  `connect_tunnel` always uses the `quinn` HTTP/3 backend;
* HTTP/2 (TCP/443) MASQUE transport deleted: `crates/usque-masque/src/h2.rs`,
  `Transport::H2`, `ConnectOptions::use_http2`, `connect_h2*` exports,
  `datagram::encode_h2_datagram_capsule_into`, feature `http2` and the `h2`
  dependency. Removed the `--http2` CLI flag and the `use_http2` parameter of
  `usque_config::select_endpoint` (which now always returns the UDP/443
  endpoint; `endpoint_h2_*` remain in the config struct for file-format
  compatibility only).
* `ConnectOptions` no longer carries `quiche_credentials`; the temporary
  cert/key files written for tokio-quiche (`usque-crypto/src/quiche.rs`) are gone.

Rationale: `h2` + the `hyper`/`tower` client stack costs several hundred kB and
UDP/443 MASQUE was verified to work through the target networks (iPhone
tethering), so HTTP/2 is not worth its size on a 3.3 MB overlay.

`vendor/h3.patch` additionally teaches the patched `h3` crate the
standards-track RFC 9484 `:protocol` token `connect-ip` (next to Cloudflare's
legacy `cf-connect-ip`), so the client can talk to self-hosted MASQUE
CONNECT-IP servers (see §8).

## 6. Registration HTTP client instead of `reqwest`

`reqwest` drags in `hyper` (client), `tower`, `tower-http`, `url`, `idna` and
the ICU data tables — well over half a megabyte of `.text` on armv7 for two
JSON calls. Replaced with a purpose-built client.

| File | Change |
| --- | --- |
| `crates/usque-cloudflare-api/src/http.rs` | new: HTTPS/1.1 client over `tokio-rustls` with a compile-time Mozilla root store (`webpki-roots`), minimal URL parser, `Content-Length`/`chunked`/EOF response bodies, 60 s timeout, 1 MiB response cap |
| `crates/usque-cloudflare-api/src/client.rs` | `CloudflareClient` wraps `HttpsClient`; `ApiError.status` is now `u16`; requests/responses use `serde_json` + `Vec<u8>` directly |

The client is only used for `api.cloudflareclient.com` (register + enroll). It
validates that endpoint with the standard web PKI (unlike the MASQUE tunnel,
which pins the endpoint key), and needs **no** `ca-bundle` package because the
roots are compiled in.

## 7. Smaller dependencies

| Change | Rationale |
| --- | --- |
| `tracing-subscriber` with `default-features = false, features = ["fmt", "std", "tracing-log"]`; `EnvFilter` replaced by a small `RUST_LOG` parser in `crates/usque-cli/src/main.rs` | drops `regex-automata`/`matchers` (~ several hundred kB). Supported syntax: a global level and/or `usque[=…]=<level>` directives |
| `clap` with `default-features = false, features = ["derive", "std", "help", "usage"]` | drops `anstream`/`anstyle`/colour/suggestion machinery |
| `chrono` replaced by a 25-line UTC formatter (`civil_from_days`) in `crates/usque-cloudflare-api/src/client.rs` | one timestamp (`tos` field, now `+00:00` instead of local offset; same RFC 3339 instant) |
| `tokio` without `full`: `rt-multi-thread, net, time, sync, macros, io-util, process` | only those are used |
| release profile `lto = "fat"`, `codegen-units = 1`, `opt-level = "z"`, `panic = "abort"`, `strip = "symbols"` | size |

## 8. CLI additions for LuCI

All existing flag names/short options are unchanged; the proxy subcommands are
gone (see §4).

* `usque --config FILE register --accept-tos --force [-n NAME] [--json]`
  * `--force` overwrites an existing config without prompting;
  * when stdin is not a TTY the command never prompts: it fails with an
    actionable message unless `--force` is given, and the Terms-of-Service
    prompt in `usque_cloudflare_api::register` is likewise replaced by an error
    unless `--accept-tos` is passed;
  * `--json` prints a one-line JSON summary (`ok`, `config`, `id`, `ipv4`,
    `ipv6`, `endpoint_v4`, `endpoint_v6`) for scripts/LuCI.
* `usque --config FILE enroll [-n NAME] [--regen-key] [--json]`:
  `--json` as above; the "invalid key, regenerate?" prompt now fails
  non-interactively with a message telling the caller to pass `--regen-key`.
* `usque --config FILE native-tun -n IFACE [...] --auto-register`:
  registers a new client (TOS auto-accepted) when the config file is missing.
* Non-Cloudflare endpoints (all defaults keep current behaviour):
  * `--authority <host|uri>` (default `https://cloudflareaccess.com`) sets the
    extended CONNECT `:authority`; a bare host is normalised to
    `https://<host>`;
  * `--protocol <token>` (default `cf-connect-ip`) selects the extended
    CONNECT `:protocol`: `cf-connect-ip` for Cloudflare, `connect-ip` for
    RFC 9484 servers;
  * together with the existing `-s/--sni-address` and `-P/--connect-port` and a
    config whose `endpoint_v4`/`endpoint_v6` point at the server and whose
    `endpoint_pub_key` pins its certificate SPKI, no other field is
    Cloudflare-specific. The endpoint must be an IP literal (the config schema
    has no DNS field), so a hostname has to be resolved before writing the
    config.

## 9. Build/portability fixes

| File | Change |
| --- | --- |
| `crates/usque-tun-platform/src/lib.rs` | `mod linux` is `#[cfg(target_os = "linux")]` (it was unconditional, so the crate did not compile on macOS even though an `unsupported` fallback exists) |
| `vendor/prepare_h3.sh` | honour `$CARGO_HOME` instead of hard-coding `$HOME/.cargo`, so the workspace-local toolchain cache works |
| `crates/usque-tun-platform/src/linux.rs` | link configuration is now idempotent: `EEXIST` from `RTM_NEWLINK`/`RTM_NEWADDR` (e.g. restarting while the persisted TUN device and its addresses are still present) is treated as "already configured" instead of a fatal error, which previously made every other procd restart exit and respawn 10 s later. The errno classification lives in `is_already_configured()` (`usque-tun-platform/src/lib.rs`) so it is unit-tested on any host |
| `crates/usque-tun-platform/src/lib.rs` | `is_already_configured(&io::Error)` helper + unit tests (EEXIST accepted, EPERM/ENOENT/EINVAL still fatal) |

## 10. Out-of-tree build machinery (not a source patch)

* `build/build-usque.sh` — single build entry point: cross-compiles
  `usque-cli` for `armv7-unknown-linux-musleabihf` with `zig` +
  `cargo-zigbuild` (or a musl.cc `CROSS_COMPILE` toolchain), strips, prints
  `file`/size/sha256, fails above 3.2 MiB, and writes `dist/usque`.
* `.github/workflows/build.yml` — runs the same script on `ubuntu-latest`,
  uploads artifact `usque-armv7-musl`.
