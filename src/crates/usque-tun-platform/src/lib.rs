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
