// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! A converted screenshot keeps its EXIF. The block is read from the source's
//! own bytes (a TIFF's IFD0 with its Exif, GPS and interoperability IFDs, or
//! the `Exif` item of a HEIF or AVIF), rebuilt as a standalone TIFF structure,
//! and written into the PNG as an `eXIf` chunk before the first `IDAT`, as
//! PNG 1.5 puts it. Best effort: a source with no EXIF, or one that doesn't
//! read, leaves the PNG as it was.

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_ENTRIES: usize = 1_000;
const MAX_DEPTH: u8 = 3;

const EXIF_IFD: u16 = 34_665;
const GPS_IFD: u16 = 34_853;
const INTEROP_IFD: u16 = 40_965;

/// IFD0 tags that describe the image's own pixels and storage, or that the
/// PNG carries another way (XMP, the ICC profile). They don't go in `eXIf`.
const IMAGE_TAGS: &[u16] = &[
    254, 255, 256, 257, 258, 259, 262, 263, 266, 273, 277, 278, 279, 280, 281, 284, 288, 289, 290,
    291, 292, 293, 317, 318, 319, 320, 321, 322, 323, 324, 325, 330, 332, 338, 339, 340, 341, 512,
    513, 514, 515, 517, 518, 519, 520, 521, 529, 530, 531, 532, 700, 34_675,
];

/// The PNG with the source's EXIF in an `eXIf` chunk. Unchanged when the PNG
/// already has one, or the source has none that reads.
pub fn carry_exif(source: &[u8], png: Vec<u8>) -> Vec<u8> {
    if png_chunks(&png).iter().any(|(kind, _, _)| kind == b"eXIf") {
        return png;
    }
    match exif_blob(source).and_then(|blob| insert_before_idat(&png, *b"eXIf", &blob)) {
        Some(with) => with,
        None => png,
    }
}

/// The source's EXIF as a TIFF-structured blob (header and IFDs, no `Exif\0\0`).
pub fn exif_blob(source: &[u8]) -> Option<Vec<u8>> {
    if source.starts_with(b"II*\0") || source.starts_with(b"MM\0*") {
        return tiff_exif(source);
    }
    if source.get(4..8) == Some(b"ftyp") {
        return heif_exif(source).and_then(|tiff| tiff_exif(&tiff));
    }
    None
}

// ── PNG chunks ──────────────────────────────────────────────────────────────

/// (type, start of the chunk, end of the chunk) for each chunk in order.
fn png_chunks(png: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    if !png.starts_with(PNG_SIGNATURE) {
        return out;
    }
    let mut at = 8usize;
    while let Some(header) = png.get(at..at + 8) {
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let kind = [header[4], header[5], header[6], header[7]];
        let Some(end) = at.checked_add(12 + len).filter(|end| *end <= png.len()) else {
            break;
        };
        out.push((kind, at, end));
        at = end;
        if kind == *b"IEND" {
            break;
        }
    }
    out
}

/// One chunk: length, type, data and the CRC of type and data.
pub fn chunk(kind: [u8; 4], data: &[u8]) -> Option<Vec<u8>> {
    let len = u32::try_from(data.len())
        .ok()
        .filter(|len| *len <= 0x7fff_ffff)?;
    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&out[4..]).to_be_bytes());
    Some(out)
}

/// The PNG with a chunk inserted just before its first `IDAT`.
pub fn insert_before_idat(png: &[u8], kind: [u8; 4], data: &[u8]) -> Option<Vec<u8>> {
    let (_, at, _) = png_chunks(png).into_iter().find(|(k, _, _)| k == b"IDAT")?;
    let piece = chunk(kind, data)?;
    let mut out = Vec::with_capacity(png.len() + piece.len());
    out.extend_from_slice(&png[..at]);
    out.extend_from_slice(&piece);
    out.extend_from_slice(&png[at..]);
    Some(out)
}

/// CRC-32 as PNG defines it (ISO 3309, reflected 0xEDB88320).
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

// ── TIFF ────────────────────────────────────────────────────────────────────

struct Tiff<'a> {
    bytes: &'a [u8],
    little: bool,
}

impl Tiff<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let b = self.bytes.get(at..at + 2)?;
        Some(if self.little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b = self.bytes.get(at..at + 4)?;
        Some(if self.little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    }
}

fn type_size(kind: u16) -> Option<usize> {
    match kind {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    /// The value's bytes as stored, in the source's byte order.
    value: Vec<u8>,
    child: Option<Ifd>,
}

struct Ifd {
    entries: Vec<Entry>,
}

fn child_tags(depth: u8) -> &'static [u16] {
    match depth {
        0 => &[EXIF_IFD, GPS_IFD],
        1 => &[INTEROP_IFD],
        _ => &[],
    }
}

fn read_ifd(tiff: &Tiff<'_>, offset: usize, depth: u8, seen: &mut Vec<usize>) -> Option<Ifd> {
    if depth > MAX_DEPTH || seen.contains(&offset) {
        return None;
    }
    seen.push(offset);
    let count = usize::from(tiff.u16(offset)?);
    if count > MAX_ENTRIES {
        return None;
    }
    let mut entries = Vec::new();
    for index in 0..count {
        let at = offset + 2 + index * 12;
        let (Some(tag), Some(kind), Some(n)) = (tiff.u16(at), tiff.u16(at + 2), tiff.u32(at + 4))
        else {
            return None;
        };
        if depth == 0 && IMAGE_TAGS.contains(&tag) {
            continue;
        }
        if child_tags(depth).contains(&tag) {
            if let Some(child) = tiff
                .u32(at + 8)
                .and_then(|to| read_ifd(tiff, to as usize, depth + 1, seen))
            {
                entries.push(Entry {
                    tag,
                    kind: 4,
                    count: 1,
                    value: Vec::new(),
                    child: Some(child),
                });
            }
            continue;
        }
        let Some(size) = type_size(kind).and_then(|s| s.checked_mul(n as usize)) else {
            continue;
        };
        let value = if size <= 4 {
            tiff.bytes.get(at + 8..at + 8 + size)
        } else {
            tiff.u32(at + 8).and_then(|from| {
                tiff.bytes
                    .get(from as usize..(from as usize).checked_add(size)?)
            })
        };
        let Some(value) = value else { continue };
        entries.push(Entry {
            tag,
            kind,
            count: n,
            value: value.to_vec(),
            child: None,
        });
    }
    entries.sort_by_key(|entry| entry.tag);
    Some(Ifd { entries })
}

fn put_u16(out: &mut [u8], at: usize, value: u16, little: bool) {
    let b = if little {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    };
    out[at..at + 2].copy_from_slice(&b);
}

fn put_u32(out: &mut [u8], at: usize, value: u32, little: bool) {
    let b = if little {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    };
    out[at..at + 4].copy_from_slice(&b);
}

/// Write one IFD and everything it points to; returns where it starts.
fn write_ifd(out: &mut Vec<u8>, ifd: &Ifd, little: bool) -> Option<u32> {
    if out.len() % 2 == 1 {
        out.push(0);
    }
    let start = out.len();
    out.resize(start + 2 + ifd.entries.len() * 12 + 4, 0);
    put_u16(out, start, u16::try_from(ifd.entries.len()).ok()?, little);
    for (index, entry) in ifd.entries.iter().enumerate() {
        let at = start + 2 + index * 12;
        put_u16(out, at, entry.tag, little);
        put_u16(out, at + 2, entry.kind, little);
        put_u32(out, at + 4, entry.count, little);
        if let Some(child) = &entry.child {
            let to = write_ifd(out, child, little)?;
            put_u32(out, at + 8, to, little);
        } else if entry.value.len() <= 4 {
            out[at + 8..at + 8 + entry.value.len()].copy_from_slice(&entry.value);
        } else {
            if out.len() % 2 == 1 {
                out.push(0);
            }
            let to = u32::try_from(out.len()).ok()?;
            out.extend_from_slice(&entry.value);
            put_u32(out, at + 8, to, little);
        }
    }
    u32::try_from(start).ok()
}

/// A TIFF's EXIF as its own TIFF structure: IFD0 without the image's storage
/// tags, and its Exif, GPS and interoperability IFDs. No thumbnail IFD.
fn tiff_exif(source: &[u8]) -> Option<Vec<u8>> {
    let little = source.starts_with(b"II");
    let tiff = Tiff {
        bytes: source,
        little,
    };
    if tiff.u16(2)? != 42 {
        return None;
    }
    let first = tiff.u32(4)? as usize;
    let ifd = read_ifd(&tiff, first, 0, &mut Vec::new())?;
    if ifd.entries.is_empty() {
        return None;
    }
    let mut out = if little {
        b"II*\0".to_vec()
    } else {
        b"MM\0*".to_vec()
    };
    out.extend_from_slice(&[0; 4]);
    let at = write_ifd(&mut out, &ifd, little)?;
    put_u32(&mut out, 4, at, little);
    Some(out)
}

// ── HEIF and AVIF ───────────────────────────────────────────────────────────

/// (type, start of payload, end) of each box in `bytes[from..to]`.
fn boxes(bytes: &[u8], from: usize, to: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut at = from;
    while at + 8 <= to {
        let size = u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        let kind = [bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]];
        let (header, size) = match size {
            0 => (8, to - at),
            1 => {
                let Some(large) = bytes.get(at + 8..at + 16) else {
                    break;
                };
                let large = u64::from_be_bytes(large.try_into().unwrap_or([0; 8]));
                (16, usize::try_from(large).unwrap_or(usize::MAX))
            }
            n => (8, n as usize),
        };
        let Some(end) = at
            .checked_add(size)
            .filter(|end| *end <= to && size >= header)
        else {
            break;
        };
        out.push((kind, at + header, end));
        at = end;
    }
    out
}

fn be(bytes: &[u8], at: usize, width: usize) -> Option<u64> {
    let part = bytes.get(at..at.checked_add(width)?)?;
    Some(part.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
}

/// The TIFF bytes of a HEIF's `Exif` item.
fn heif_exif(bytes: &[u8]) -> Option<Vec<u8>> {
    let (_, meta_from, meta_to) = boxes(bytes, 0, bytes.len())
        .into_iter()
        .find(|(kind, _, _)| kind == b"meta")?;
    // `meta` is a full box: version and flags first.
    let children = boxes(bytes, meta_from + 4, meta_to);
    let (_, iinf_from, iinf_to) = *children.iter().find(|(k, _, _)| k == b"iinf")?;
    let iinf_version = *bytes.get(iinf_from)?;
    let entries_at = iinf_from + 4 + if iinf_version == 0 { 2 } else { 4 };
    let item = boxes(bytes, entries_at, iinf_to)
        .into_iter()
        .filter(|(k, _, _)| k == b"infe")
        .find_map(|(_, from, _)| {
            let version = *bytes.get(from)?;
            let (id, type_at) = match version {
                2 => (be(bytes, from + 4, 2)?, from + 8),
                3 => (be(bytes, from + 4, 4)?, from + 10),
                _ => return None,
            };
            (bytes.get(type_at..type_at + 4)? == b"Exif").then_some(id)
        })?;
    let (_, iloc_from, _) = *children.iter().find(|(k, _, _)| k == b"iloc")?;
    let version = *bytes.get(iloc_from)?;
    let mut at = iloc_from + 4;
    let sizes = *bytes.get(at)?;
    let more = *bytes.get(at + 1)?;
    let (offset_size, length_size) = (usize::from(sizes >> 4), usize::from(sizes & 0xf));
    let base_size = usize::from(more >> 4);
    let index_size = if version >= 1 {
        usize::from(more & 0xf)
    } else {
        0
    };
    at += 2;
    let id_size = if version < 2 { 2 } else { 4 };
    let count = be(bytes, at, id_size)?;
    at += id_size;
    for _ in 0..count.min(10_000) {
        let id = be(bytes, at, id_size)?;
        at += id_size;
        let method = if version >= 1 {
            let m = be(bytes, at, 2)? & 0xf;
            at += 2;
            m
        } else {
            0
        };
        at += 2; // data reference index
        let base = if base_size > 0 {
            be(bytes, at, base_size)?
        } else {
            0
        };
        at += base_size;
        let extents = be(bytes, at, 2)?;
        at += 2;
        let mut first = None;
        for _ in 0..extents {
            at += index_size;
            let offset = if offset_size > 0 {
                be(bytes, at, offset_size)?
            } else {
                0
            };
            at += offset_size;
            let length = be(bytes, at, length_size)?;
            at += length_size;
            first.get_or_insert((offset, length));
        }
        if id != item || method != 0 {
            continue;
        }
        let (offset, length) = first?;
        let from = usize::try_from(base.checked_add(offset)?).ok()?;
        let payload = bytes.get(from..from.checked_add(usize::try_from(length).ok()?)?)?;
        // The payload starts with where the TIFF header is, after itself.
        let skip = usize::try_from(be(payload, 0, 4)?).ok()?;
        return payload.get(4usize.checked_add(skip)?..).map(<[u8]>::to_vec);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le16(v: u16) -> [u8; 2] {
        v.to_le_bytes()
    }
    fn le32(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    /// A 2 by 1 RGB TIFF with Make in IFD0 and DateTimeOriginal in its Exif IFD.
    fn tiff_with_make(make: &str) -> Vec<u8> {
        let make = format!("{make}\0");
        let date = "2026:11:13 01:45:42\0";
        let entries: u16 = 10;
        let ifd0 = 8u32;
        let ifd0_end = ifd0 + 2 + u32::from(entries) * 12 + 4;
        let make_at = ifd0_end;
        let bits_at = make_at + make.len() as u32;
        let exif_at = bits_at + 6 + (bits_at + 6) % 2;
        let exif_end = exif_at + 2 + 12 + 4;
        let date_at = exif_end;
        let pixels_at = date_at + date.len() as u32;
        let mut out = b"II*\0".to_vec();
        out.extend_from_slice(&le32(ifd0));
        out.extend_from_slice(&le16(entries));
        let mut entry = |tag: u16, kind: u16, count: u32, value: u32| {
            out.extend_from_slice(&le16(tag));
            out.extend_from_slice(&le16(kind));
            out.extend_from_slice(&le32(count));
            out.extend_from_slice(&le32(value));
        };
        entry(256, 4, 1, 2);
        entry(257, 4, 1, 1);
        entry(258, 3, 3, bits_at);
        entry(259, 3, 1, 1);
        entry(262, 3, 1, 2);
        entry(271, 2, make.len() as u32, make_at);
        entry(273, 4, 1, pixels_at);
        entry(277, 3, 1, 3);
        entry(279, 4, 1, 6);
        entry(34_665, 4, 1, exif_at);
        out.extend_from_slice(&le32(0));
        out.extend_from_slice(make.as_bytes());
        for _ in 0..3 {
            out.extend_from_slice(&le16(8));
        }
        while out.len() < exif_at as usize {
            out.push(0);
        }
        out.extend_from_slice(&le16(1));
        out.extend_from_slice(&le16(36_867));
        out.extend_from_slice(&le16(2));
        out.extend_from_slice(&le32(date.len() as u32));
        out.extend_from_slice(&le32(date_at));
        out.extend_from_slice(&le32(0));
        out.extend_from_slice(date.as_bytes());
        out.extend_from_slice(&[255, 0, 0, 0, 0, 255]);
        out
    }

    fn png_with_idat() -> Vec<u8> {
        let mut out = PNG_SIGNATURE.to_vec();
        out.extend(chunk(*b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 2, 0, 0, 0]).unwrap());
        out.extend(chunk(*b"iCCP", b"icc\0\0x").unwrap());
        out.extend(chunk(*b"IDAT", b"pixels").unwrap());
        out.extend(chunk(*b"IDAT", b"more").unwrap());
        out.extend(chunk(*b"IEND", b"").unwrap());
        out
    }

    #[test]
    fn the_crc_is_pngs() {
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
        assert_eq!(
            &chunk(*b"IEND", b"").unwrap()[8..],
            &[0xae, 0x42, 0x60, 0x82]
        );
    }

    #[test]
    fn exif_goes_in_before_the_first_idat_with_its_crc() {
        let png = png_with_idat();
        let out = carry_exif(&tiff_with_make("fixturemake"), png.clone());
        let kinds: Vec<[u8; 4]> = png_chunks(&out).iter().map(|(k, _, _)| *k).collect();
        assert_eq!(
            kinds,
            [*b"IHDR", *b"iCCP", *b"eXIf", *b"IDAT", *b"IDAT", *b"IEND"]
        );
        let (_, from, to) = png_chunks(&out)[2];
        let piece = &out[from..to];
        let len = u32::from_be_bytes(piece[0..4].try_into().unwrap()) as usize;
        let data = &piece[8..8 + len];
        assert_eq!(
            u32::from_be_bytes(piece[8 + len..].try_into().unwrap()),
            crc32(&piece[4..8 + len])
        );
        assert!(data.starts_with(b"II*\0"), "a TIFF header, no Exif prefix");
        assert!(data.windows(11).any(|w| w == b"fixturemake"));
        assert!(data.windows(19).any(|w| w == b"2026:11:13 01:45:42"));
        // Everything else is byte for byte where it was.
        assert_eq!(&out[..from], &png[..from]);
        assert_eq!(&out[to..], &png[from..]);
    }

    #[test]
    fn the_blob_keeps_the_exif_and_leaves_the_pixels_behind() {
        let blob = exif_blob(&tiff_with_make("fixturemake")).unwrap();
        let tiff = Tiff {
            bytes: &blob,
            little: true,
        };
        let ifd = read_ifd(&tiff, tiff.u32(4).unwrap() as usize, 0, &mut Vec::new()).unwrap();
        let tags: Vec<u16> = ifd.entries.iter().map(|e| e.tag).collect();
        assert_eq!(tags, [271, EXIF_IFD], "no strips, sizes or samples");
        let exif = ifd.entries[1].child.as_ref().unwrap();
        assert_eq!(exif.entries[0].tag, 36_867);
        assert_eq!(exif.entries[0].value, b"2026:11:13 01:45:42\0");
        assert_eq!(ifd.entries[0].value, b"fixturemake\0");
    }

    #[test]
    fn a_heif_exif_item_comes_out_as_its_tiff() {
        let tiff = exif_blob(&tiff_with_make("phone")).unwrap();
        let mut payload = 6u32.to_be_bytes().to_vec();
        payload.extend_from_slice(b"Exif\0\0");
        payload.extend_from_slice(&tiff);
        let full = |kind: &[u8; 4], body: &[u8]| {
            let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            b.extend_from_slice(kind);
            b.extend_from_slice(body);
            b
        };
        let mut infe = vec![2, 0, 0, 0, 0, 7, 0, 0];
        infe.extend_from_slice(b"Exif");
        infe.push(0);
        let mut iinf = vec![0, 0, 0, 0, 0, 1];
        iinf.extend(full(b"infe", &infe));
        let ftyp = full(b"ftyp", b"heic\0\0\0\0mif1heic");
        // iloc v0, offset 4 bytes, length 4 bytes, no base offset.
        let iloc_len = 8 + 4 + 2 + 2 + 2 + 2 + 2 + 8;
        let meta_len = 8 + 4 + (8 + iinf.len()) + iloc_len;
        let data_at = (ftyp.len() + meta_len) as u32;
        let mut iloc = vec![0, 0, 0, 0, 0x44, 0x00, 0, 1, 0, 7, 0, 0, 0, 1];
        iloc.extend_from_slice(&data_at.to_be_bytes());
        iloc.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        let mut meta = vec![0, 0, 0, 0];
        meta.extend(full(b"iinf", &iinf));
        meta.extend(full(b"iloc", &iloc));
        let mut file = ftyp;
        file.extend(full(b"meta", &meta));
        assert_eq!(file.len() as u32, data_at);
        file.extend_from_slice(&payload);
        let blob = exif_blob(&file).unwrap();
        assert!(blob.windows(5).any(|w| w == b"phone"));
    }

    #[test]
    fn no_exif_or_garbage_leaves_the_png_as_it_was() {
        let png = png_with_idat();
        assert_eq!(carry_exif(b"GIF89a", png.clone()), png);
        assert_eq!(carry_exif(b"II*\0\xff\xff\xff\xff", png.clone()), png);
        assert_eq!(
            carry_exif(b"MM\0*\0\0\0\x08\0\0", png.clone()),
            png,
            "an empty IFD0"
        );
        let mut heif = b"\0\0\0\x10ftypheic".to_vec();
        heif.extend_from_slice(&[0; 4]);
        assert_eq!(carry_exif(&heif, png.clone()), png);
        assert_eq!(
            carry_exif(&tiff_with_make("x"), b"not a png".to_vec()),
            b"not a png"
        );
        // One eXIf only: a PNG that has one keeps it.
        let once = carry_exif(&tiff_with_make("first"), png);
        assert_eq!(carry_exif(&tiff_with_make("second"), once.clone()), once);
    }
}
