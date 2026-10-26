#[cfg(windows)]
use anyhow::{Context, Result};
#[cfg(windows)]
use async_trait::async_trait;
#[cfg(windows)]
use bytes::{Bytes, BytesMut};
#[cfg(windows)]
use std::process::Command;
#[cfg(windows)]
use std::sync::Arc;
#[cfg(windows)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(windows)]
use tun::{AsyncDevice, Configuration};

#[cfg(windows)]
use crate::{NativeTun, NativeTunConfig};
#[cfg(windows)]
use usque_tunnel_core::TunnelDevice;

#[cfg(windows)]
struct TunAsyncDevice(Arc<tokio::sync::Mutex<AsyncDevice>>);

#[cfg(windows)]
#[async_trait]
impl TunnelDevice for TunAsyncDevice {
    async fn read_packet(&self, buf: &mut BytesMut) -> std::io::Result<usize> {
        // Read into the spare capacity, then commit with `set_len`.
        // See the Linux impl for the rationale.
        let spare = buf.spare_capacity_mut();
        let dst =
            unsafe { std::slice::from_raw_parts_mut(spare.as_mut_ptr().cast::<u8>(), spare.len()) };
        let n = {
            let mut dev = self.0.lock().await;
            dev.read(dst).await?
        };
        unsafe {
            buf.set_len(n);
        }
        Ok(n)
    }

    async fn write_packet(&self, packet: Bytes) -> std::io::Result<()> {
        let mut dev = self.0.lock().await;
        dev.write_all(&packet).await?;
        Ok(())
    }
}

#[cfg(windows)]
pub async fn create(cfg: NativeTunConfig) -> Result<NativeTun> {
    let name = if cfg.name.is_empty() {
        "usque".to_string()
    } else {
        cfg.name.clone()
    };

    let mut config = Configuration::default();
    config.name(&name).mtu(cfg.mtu as i32);
    let dev = tun::create_as_async(&config).context("failed to create TUN device")?;
    let iface_name = dev
        .get_ref()
        .name()
        .context("failed to get interface name")?
        .to_string();

    if cfg.configure_link {
        if let Some(v4) = cfg.ipv4.as_deref() {
            set_ipv4_address(&iface_name, v4, "255.255.255.255")?;
            set_ipv4_mtu(&iface_name, cfg.mtu)?;
        }
        if let Some(v6) = cfg.ipv6.as_deref() {
            set_ipv6_address(&iface_name, v6, "128")?;
            set_ipv6_mtu(&iface_name, cfg.mtu)?;
        }
    }

    Ok(NativeTun {
        device: Box::new(TunAsyncDevice(Arc::new(tokio::sync::Mutex::new(dev)))),
        name: iface_name,
    })
}

#[cfg(windows)]
fn set_ipv4_address(iface: &str, ip: &str, mask: &str) -> Result<()> {
    let output = Command::new("netsh")
        .args([
            "interface",
            "ipv4",
            "set",
            "address",
            &format!("name=\"{iface}\""),
            "static",
            ip,
            mask,
        ])
        .output()
        .context("netsh failed")?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

#[cfg(windows)]
fn set_ipv6_address(iface: &str, ip: &str, prefix: &str) -> Result<()> {
    let output = Command::new("netsh")
        .args([
            "interface",
            "ipv6",
            "set",
            "address",
            &format!("interface=\"{iface}\""),
            &format!("{ip}/{prefix}"),
        ])
        .output()
        .context("netsh failed")?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

#[cfg(windows)]
fn set_ipv4_mtu(iface: &str, mtu: usize) -> Result<()> {
    let output = Command::new("netsh")
        .args([
            "interface",
            "ipv4",
            "set",
            "subinterface",
            &format!("\"{iface}\""),
            &format!("mtu={mtu}"),
        ])
        .output()
        .context("netsh failed")?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

#[cfg(windows)]
fn set_ipv6_mtu(iface: &str, mtu: usize) -> Result<()> {
    let output = Command::new("netsh")
        .args([
            "interface",
            "ipv6",
            "set",
            "subinterface",
            &format!("\"{iface}\""),
            &format!("mtu={mtu}"),
        ])
        .output()
        .context("netsh failed")?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}
