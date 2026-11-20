//! 最小 ZIP 写入器：只做 store（不压缩）。
//!
//! 漫画页本身就是 JPEG/WebP，再 deflate 收益接近零，却要多耗 KPW3 的 CPU；
//! EPUB 的 `mimetype` 也要求不压缩。因此这里只实现 store，并保持输出确定性：
//! 固定 1980-01-01 时间戳、UTF-8 文件名、条目顺序即写入顺序。
//!
//! 局限（明确报错，不静默产出坏包）：单文件/总偏移 ≤ 4 GiB、条目数 ≤ 65535。
//! 超过时请按卷分批导出。

use tokio::io::{AsyncWrite, AsyncWriteExt};

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
/// UTF-8 文件名标志位。
const FLAG_UTF8: u16 = 0x0800;
/// DOS 日期 1980-01-01（年偏移 0、月 1、日 1）。
const DOS_DATE: u16 = 0x0021;
const DOS_TIME: u16 = 0;

struct Entry {
    name: String,
    crc: u32,
    size: u32,
    offset: u32,
}

/// store-only ZIP 写入器。
pub struct ZipWriter<W> {
    w: W,
    entries: Vec<Entry>,
    offset: u64,
}

impl<W: AsyncWrite + Unpin> ZipWriter<W> {
    pub fn new(w: W) -> Self {
        Self {
            w,
            entries: Vec::new(),
            offset: 0,
        }
    }

    /// 追加一个不压缩的条目。
    pub async fn add_stored(&mut self, name: &str, data: &[u8]) -> anyhow::Result<()> {
        let name_bytes = name.as_bytes();
        if name_bytes.is_empty() {
            anyhow::bail!("ZIP 条目名不能为空");
        }
        if name_bytes.len() > u16::MAX as usize {
            anyhow::bail!("ZIP 条目名过长：{}", name);
        }
        if self.entries.len() >= u16::MAX as usize {
            anyhow::bail!("ZIP 条目超过 65535，请按卷分批导出");
        }
        let size = u32::try_from(data.len())
            .map_err(|_| anyhow::anyhow!("单页超过 4 GiB，无法写入 CBZ"))?;
        let offset = u32::try_from(self.offset)
            .map_err(|_| anyhow::anyhow!("CBZ 已超过 4 GiB，请按卷分批导出"))?;

        let crc = crc32(data);
        let mut header = Vec::with_capacity(30 + name_bytes.len());
        header.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        header.extend_from_slice(&20u16.to_le_bytes()); // version needed
        header.extend_from_slice(&FLAG_UTF8.to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // method: store
        header.extend_from_slice(&DOS_TIME.to_le_bytes());
        header.extend_from_slice(&DOS_DATE.to_le_bytes());
        header.extend_from_slice(&crc.to_le_bytes());
        header.extend_from_slice(&size.to_le_bytes()); // compressed size
        header.extend_from_slice(&size.to_le_bytes()); // uncompressed size
        header.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // extra len
        header.extend_from_slice(name_bytes);

        self.w.write_all(&header).await?;
        self.w.write_all(data).await?;
        self.offset += header.len() as u64 + data.len() as u64;

        self.entries.push(Entry {
            name: name.to_string(),
            crc,
            size,
            offset,
        });
        Ok(())
    }

    /// 写中央目录与 EOCD，返回文件总字节数。
    pub async fn finish(mut self) -> anyhow::Result<u64> {
        let cd_offset = u32::try_from(self.offset)
            .map_err(|_| anyhow::anyhow!("CBZ 已超过 4 GiB，请按卷分批导出"))?;
        let mut central = Vec::new();
        for e in &self.entries {
            let name = e.name.as_bytes();
            central.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes()); // version made by
            central.extend_from_slice(&20u16.to_le_bytes()); // version needed
            central.extend_from_slice(&FLAG_UTF8.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // method: store
            central.extend_from_slice(&DOS_TIME.to_le_bytes());
            central.extend_from_slice(&DOS_DATE.to_le_bytes());
            central.extend_from_slice(&e.crc.to_le_bytes());
            central.extend_from_slice(&e.size.to_le_bytes());
            central.extend_from_slice(&e.size.to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // extra
            central.extend_from_slice(&0u16.to_le_bytes()); // comment
            central.extend_from_slice(&0u16.to_le_bytes()); // disk number
            central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            central.extend_from_slice(&e.offset.to_le_bytes());
            central.extend_from_slice(name);
        }
        let count = self.entries.len() as u16;
        let cd_size =
            u32::try_from(central.len()).map_err(|_| anyhow::anyhow!("ZIP 中央目录超过 4 GiB"))?;

        let mut eocd = Vec::with_capacity(22);
        eocd.extend_from_slice(&EOCD_SIG.to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes()); // disk
        eocd.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
        eocd.extend_from_slice(&count.to_le_bytes());
        eocd.extend_from_slice(&count.to_le_bytes());
        eocd.extend_from_slice(&cd_size.to_le_bytes());
        eocd.extend_from_slice(&cd_offset.to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes()); // comment len

        self.w.write_all(&central).await?;
        self.w.write_all(&eocd).await?;
        self.w.flush().await?;
        Ok(self.offset + central.len() as u64 + eocd.len() as u64)
    }
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

/// 标准 CRC-32（IEEE 802.3），ZIP 与 PNG 共用。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = CRC_TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "oread-zip-{tag}-{}-{stamp}.zip",
            std::process::id()
        ))
    }

    fn u16_at(b: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([b[at], b[at + 1]])
    }

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    #[test]
    fn crc32_matches_known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"hello"), 0x3610_a686);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[tokio::test]
    async fn writes_stored_entries_and_central_directory() {
        let path = temp_path("basic");
        let mut zip = ZipWriter::new(tokio::fs::File::create(&path).await.unwrap());
        zip.add_stored("001-001.jpg", b"jpeg-one").await.unwrap();
        zip.add_stored("ComicInfo.xml", b"<ComicInfo/>")
            .await
            .unwrap();
        let total = zip.finish().await.unwrap();

        let bytes = tokio::fs::read(&path).await.unwrap();
        let _ = tokio::fs::remove_file(&path).await;
        assert_eq!(bytes.len() as u64, total);

        // 第一个本地头：store、UTF-8、名字正确、CRC 正确。
        assert_eq!(u32_at(&bytes, 0), LOCAL_SIG);
        assert_eq!(u16_at(&bytes, 6), FLAG_UTF8);
        assert_eq!(u16_at(&bytes, 8), 0);
        let name_len = u16_at(&bytes, 26) as usize;
        assert_eq!(&bytes[30..30 + name_len], b"001-001.jpg");
        assert_eq!(u32_at(&bytes, 14), crc32(b"jpeg-one"));
        assert_eq!(u32_at(&bytes, 18), 8);
        assert_eq!(u32_at(&bytes, 22), 8);

        // EOCD 在末尾，条目数 = 2。
        let eocd = bytes.len() - 22;
        assert_eq!(u32_at(&bytes, eocd), EOCD_SIG);
        assert_eq!(u16_at(&bytes, eocd + 8), 2);
        assert_eq!(u16_at(&bytes, eocd + 10), 2);
        let cd_offset = u32_at(&bytes, eocd + 16) as usize;
        assert_eq!(u32_at(&bytes, cd_offset), CENTRAL_SIG);
    }

    #[tokio::test]
    async fn rejects_oversized_entry_count() {
        let path = temp_path("count");
        let mut zip = ZipWriter::new(tokio::fs::File::create(&path).await.unwrap());
        // 直接塞满 entries 避免真的写 6.5 万个条目
        for i in 0..u16::MAX as usize {
            zip.entries.push(Entry {
                name: format!("{i}"),
                crc: 0,
                size: 0,
                offset: 0,
            });
        }
        let err = zip.add_stored("x", b"").await.unwrap_err();
        assert!(err.to_string().contains("65535"));
        drop(zip);
        let _ = tokio::fs::remove_file(&path).await;
    }
}
