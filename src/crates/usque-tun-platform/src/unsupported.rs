use anyhow::{bail, Result};

use crate::{NativeTun, NativeTunConfig};

pub async fn create(_cfg: NativeTunConfig) -> Result<NativeTun> {
    bail!("native TUN is not supported on this platform")
}
