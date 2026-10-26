#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(any(target_os = "linux", windows)))]
mod unsupported;
#[cfg(windows)]
mod windows;

use anyhow::Result;
use async_trait::async_trait;
use bytes::BytesMut;

use usque_tunnel_core::TunnelDevice;

pub struct NativeTunConfig {
    pub name: String,
    pub mtu: usize,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    pub configure_link: bool,
    pub persist: bool,
}

pub struct NativeTun {
    device: Box<dyn TunnelDevice + Send>,
    pub name: String,
}

impl NativeTun {
    pub async fn create(cfg: NativeTunConfig) -> Result<Self> {
        platform_create(cfg).await
    }
}

#[async_trait]
impl TunnelDevice for NativeTun {
    async fn read_packet(&self, buf: &mut BytesMut) -> std::io::Result<usize> {
        self.device.read_packet(buf).await
    }

    async fn write_packet(&self, packet: bytes::Bytes) -> std::io::Result<()> {
        self.device.write_packet(packet).await
    }
}

/// Whether an OS error just means "the requested state is already in place".
///
/// Restarting the daemon while a (persisted) TUN device is still present makes
/// the kernel answer `RTM_NEWADDR`/`RTM_NEWLINK` with `EEXIST` for the address
/// and link settings that are already configured. That is the desired end
/// state, not a failure: configure the link idempotently and keep going.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn is_already_configured(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::AlreadyExists
}

#[cfg(target_os = "linux")]
async fn platform_create(cfg: NativeTunConfig) -> Result<NativeTun> {
    linux::create(cfg).await
}

#[cfg(windows)]
async fn platform_create(cfg: NativeTunConfig) -> Result<NativeTun> {
    windows::create(cfg).await
}

#[cfg(not(any(target_os = "linux", windows)))]
async fn platform_create(cfg: NativeTunConfig) -> Result<NativeTun> {
    unsupported::create(cfg).await
}

#[cfg(test)]
mod tests {
    use super::is_already_configured;

    #[test]
    fn eexist_counts_as_already_configured() {
        // Linux EEXIST == 17; `io::ErrorKind::AlreadyExists` is what both the
        // raw-os-error and the kind-constructed variants report.
        assert!(is_already_configured(&std::io::Error::from_raw_os_error(17)));
        assert!(is_already_configured(&std::io::Error::from(
            std::io::ErrorKind::AlreadyExists
        )));
    }

    #[test]
    fn other_errors_are_still_fatal() {
        // EPERM (1), ENOENT (2), EINVAL (22): must not be swallowed.
        for code in [1, 2, 22, 100] {
            assert!(
                !is_already_configured(&std::io::Error::from_raw_os_error(code)),
                "errno {code} must not be treated as already configured"
            );
        }
        assert!(!is_already_configured(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
    }
}
