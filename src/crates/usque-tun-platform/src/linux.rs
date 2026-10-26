use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tun::{AbstractDevice, AsyncDevice, Configuration, DeviceReader, DeviceWriter};

use crate::{NativeTun, NativeTunConfig};
use usque_tunnel_core::TunnelDevice;

/// Split halves of the TUN file descriptor, each wrapped in its own
/// `AsyncFd`. The previous design wrapped the whole `AsyncDevice` in
/// a shared `tokio::sync::Mutex`, which coupled the two directions:
/// `read_packet` held the lock across the entire `read().await`,
/// which pends until a packet arrives from the kernel — so an
/// inbound packet write stalled behind every pending read (and vice
/// versa). The kernel already serializes `read`/`write` on one fd,
/// and `DeviceReader`/`DeviceWriter` each keep their own readiness
/// registration, so the mutex is unnecessary on Linux.
struct TunAsyncDevice {
    /// Guarded only because `AsyncReadExt` needs `&mut`; the
    /// supervisor reads from a single task, so this mutex is never
    /// contended and — crucially — no longer couples the write path
    /// (the old design shared one mutex between both directions, so
    /// every pending read blocked inbound writes).
    reader: tokio::sync::Mutex<DeviceReader>,
    writer: DeviceWriter,
}

#[async_trait]
impl TunnelDevice for TunAsyncDevice {
    async fn read_packet(&self, buf: &mut BytesMut) -> io::Result<usize> {
        // `DeviceReader`'s `AsyncRead` impl expects a `&mut [u8]`
        // slice. The supervisor's scratch `BytesMut` has `len == 0`
        // and `cap == MTU`, so dereffing gives a zero-length slice.
        // Read into the spare capacity instead, then commit with
        // `set_len`.
        let spare = buf.spare_capacity_mut();
        let dst =
            unsafe { std::slice::from_raw_parts_mut(spare.as_mut_ptr().cast::<u8>(), spare.len()) };
        let n = self.reader.lock().await.read(dst).await?;
        unsafe {
            buf.set_len(n);
        }
        Ok(n)
    }

    async fn write_packet(&self, packet: Bytes) -> io::Result<()> {
        // `DeviceWriter` is `Clone` (cheap `Arc` bump) and polls the
        // fd readiness lock-free; no read/write coupling.
        let mut writer = self.writer.clone();
        writer.write_all(&packet).await?;
        Ok(())
    }
}

pub async fn create(cfg: NativeTunConfig) -> Result<NativeTun> {
    let mut config = Configuration::default();
    if !cfg.name.is_empty() {
        config.tun_name(&cfg.name);
    }

    let mut dev = tun::create_as_async(&config).context("failed to create TUN device")?;
    if cfg.persist {
        dev.persist().context("failed to persist TUN device")?;
    }

    let name = dev.tun_name().context("failed to get interface name")?;

    if cfg.configure_link {
        configure_link(&name, cfg.mtu, cfg.ipv4.as_deref(), cfg.ipv6.as_deref()).await?;
    }

    // `split()` returns the writer first, then the reader.
    let (writer, reader) = dev.split().context("failed to split TUN device")?;

    Ok(NativeTun {
        device: Box::new(TunAsyncDevice {
            reader: tokio::sync::Mutex::new(reader),
            writer,
        }),
        name,
    })
}

async fn configure_link(
    name: &str,
    mtu: usize,
    ipv4: Option<&str>,
    ipv6: Option<&str>,
) -> Result<()> {
    use futures::TryStreamExt;
    use rtnetlink::{new_connection, LinkUnspec};

    let (connection, handle, _) = new_connection()?;
    tokio::spawn(connection);

    let mut links = handle.link().get().match_name(name.to_string()).execute();
    let link = links
        .try_next()
        .await
        .context("failed to query link")?
        .context("link not found")?;
    let index = link.header.index;

    handle
        .link()
        .change(
            LinkUnspec::new_with_index(index)
                .mtu(mtu as u32)
                .up()
                .build(),
        )
        .execute()
        .await
        .context("failed to set link up/mtu")?;

    if let Some(v4) = ipv4 {
        let ip: std::net::Ipv4Addr = v4.parse()?;
        handle
            .address()
            .add(index, ip.into(), 32)
            .execute()
            .await
            .context("failed to add IPv4 address")?;
    }

    if let Some(v6) = ipv6 {
        let ip: std::net::Ipv6Addr = v6.parse()?;
        handle
            .address()
            .add(index, ip.into(), 128)
            .execute()
            .await
            .context("failed to add IPv6 address")?;
    }

    Ok(())
}
