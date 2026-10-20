//! Concatenated Zstandard frames as dsh writes them: one checksummed frame for
//! the header line, then one per durable append batch (`zstd.ts`).
use std::ops::Range;

use crate::error::{Result, SessionError};

const ZSTD_MAGIC: u32 = 0xFD2F_B528;
/// Node's built-in Zstandard default (`ZSTD_CLEVEL_DEFAULT`).
const LEVEL: i32 = 3;

/// Structural scan of a frame stream.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FrameScan {
    /// Complete frames in file order.
    pub frames: Vec<Range<usize>>,
    /// Start of an incomplete final frame cut by EOF.
    pub torn_start: Option<usize>,
}

/// Locates complete frames without decompressing (port of `scanZstdFrames`).
/// Invalid complete structure is corruption; EOF inside the final frame is a torn tail.
pub fn scan_frames(buf: &[u8], max_frames: usize) -> Result<FrameScan> {
    let mut scan = FrameScan::default();
    let mut offset = 0usize;
    let torn = |mut scan: FrameScan, start| {
        scan.torn_start = Some(start);
        Ok(scan)
    };
    while offset < buf.len() {
        let start = offset;
        if buf.len() - offset < 4 {
            return torn(scan, start);
        }
        let magic = u32::from_le_bytes(buf[offset..offset + 4].try_into().expect("4 bytes"));
        if magic != ZSTD_MAGIC {
            return Err(SessionError::Corrupt(format!(
                "invalid Zstandard frame magic at byte {offset}"
            )));
        }
        offset += 4;
        if offset == buf.len() {
            return torn(scan, start);
        }
        let descriptor = buf[offset];
        offset += 1;
        if descriptor & 0x18 != 0 {
            return Err(SessionError::Corrupt(format!(
                "reserved frame-header bit at byte {}",
                offset - 1
            )));
        }
        let content_size_flag = descriptor >> 6;
        let single_segment = descriptor & 0x20 != 0;
        let checksum = descriptor & 0x04 != 0;
        let dictionary_bytes = match descriptor & 0x03 {
            3 => 4,
            n => n as usize,
        };
        let content_size_bytes = match content_size_flag {
            0 => usize::from(single_segment),
            n => 1 << n,
        };
        let remaining = usize::from(!single_segment) + dictionary_bytes + content_size_bytes;
        if buf.len() - offset < remaining {
            return torn(scan, start);
        }
        offset += remaining;
        loop {
            if buf.len() - offset < 3 {
                return torn(scan, start);
            }
            let header = u32::from(buf[offset])
                | u32::from(buf[offset + 1]) << 8
                | u32::from(buf[offset + 2]) << 16;
            offset += 3;
            let last = header & 1 != 0;
            let block_type = (header >> 1) & 0x03;
            let block_size = (header >> 3) as usize;
            if block_type == 3 {
                return Err(SessionError::Corrupt(format!(
                    "reserved block type at byte {}",
                    offset - 3
                )));
            }
            let payload = if block_type == 1 { 1 } else { block_size };
            if buf.len() - offset < payload {
                return torn(scan, start);
            }
            offset += payload;
            if last {
                break;
            }
        }
        if checksum {
            if buf.len() - offset < 4 {
                return torn(scan, start);
            }
            offset += 4;
        }
        scan.frames.push(start..offset);
        if scan.frames.len() == max_frames {
            return Ok(scan);
        }
    }
    Ok(scan)
}

/// One independently decodable, checksummed frame without a content-size
/// field (the streaming shape Node's `zstdCompress` produces).
pub fn compress_frame(plaintext: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder =
        zstd::stream::write::Encoder::new(Vec::with_capacity(plaintext.len() / 3 + 64), LEVEL)
            .expect("zstd encoder");
    encoder.include_checksum(true).expect("zstd checksum flag");
    encoder.write_all(plaintext).expect("in-memory zstd write");
    encoder.finish().expect("in-memory zstd finish")
}

/// Decompresses one complete frame; libzstd verifies its checksum.
pub fn decompress_frame(frame: &[u8]) -> Result<Vec<u8>> {
    zstd::stream::decode_all(frame)
        .map_err(|e| SessionError::Corrupt(format!("Zstandard frame failed to decode: {e}")))
}

/// Recovers whatever plaintext an incomplete final frame yields; never fails.
pub fn decompress_prefix(input: &[u8]) -> Vec<u8> {
    use zstd::stream::raw::{Decoder, InBuffer, Operation, OutBuffer};
    let mut out = Vec::new();
    let Ok(mut decoder) = Decoder::new() else {
        return out;
    };
    let mut src = InBuffer::around(input);
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let mut dst = OutBuffer::around(chunk.as_mut_slice());
        let before = src.pos();
        match decoder.run(&mut src, &mut dst) {
            Ok(_) => {
                let written = dst.pos();
                out.extend_from_slice(&chunk[..written]);
                if written == 0 && src.pos() == before {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_have_dsh_shape_and_scan() {
        let a = compress_frame(b"{\"type\":\"session\"}\n");
        // No content size, not single-segment, checksum on: descriptor 0x04 like Node.
        assert_eq!(a[4], 0x04);
        let b = compress_frame(&b"x".repeat(100_000));
        let mut all = [a.clone(), b.clone()].concat();
        let scan = scan_frames(&all, usize::MAX).unwrap();
        assert_eq!(scan.frames, vec![0..a.len(), a.len()..a.len() + b.len()]);
        assert_eq!(scan.torn_start, None);
        assert_eq!(
            decompress_frame(&all[scan.frames[1].clone()])
                .unwrap()
                .len(),
            100_000
        );
        all.truncate(all.len() - 3);
        let scan = scan_frames(&all, usize::MAX).unwrap();
        assert_eq!(scan.frames.len(), 1);
        assert_eq!(scan.torn_start, Some(a.len()));
        all.push(0);
        all[a.len()] ^= 0xff;
        assert!(scan_frames(&all, usize::MAX).is_err());
    }

    #[test]
    fn checksum_mismatch_is_corruption() {
        let mut frame = compress_frame(b"hello hello hello hello\n");
        let n = frame.len();
        frame[n - 1] ^= 0xff;
        assert!(decompress_frame(&frame).is_err());
    }

    #[test]
    fn prefix_of_torn_frame_yields_complete_plaintext_so_far() {
        let text: Vec<u8> = (0..2000)
            .flat_map(|i| format!("{{\"line\":{i}}}\n").into_bytes())
            .collect();
        let frame = compress_frame(&text);
        let full = decompress_prefix(&frame);
        assert_eq!(full, text);
        let cut = decompress_prefix(&frame[..frame.len() - 4]);
        assert!(text.starts_with(&cut));
        assert!(decompress_prefix(&frame[..6]).is_empty());
    }
}
