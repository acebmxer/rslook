//! Synthetic Unicode `.pst` writer for tests and examples.
//!
//! Written from the public [MS-PST] specification only. It produces a small but structurally
//! complete Unicode PST (no encryption): header and root, AMap/PMap pages, node and block B-trees,
//! data trees (XBLOCK), subnode trees (SLBLOCK), heap-on-node, BTree-on-heap, property contexts
//! and table contexts. The app never writes PSTs; this exists so tests never need a file made by
//! Outlook.
//!
//! Layout written by [`write_pst`]:
//! - message store PC (NID 0x21) with record key, display name, IPM subtree, finder and (when a
//!   top-level folder is named "Deleted Items") wastebasket entry IDs;
//! - name-to-ID map PC (NID 0x61), empty;
//! - root folder (NID 0x122) with two children: "Top of Personal Folders" (the IPM subtree, which
//!   holds the caller's folders) and "Search Root";
//! - per folder: PC, hierarchy table, contents table and associated contents table;
//! - per message: PC with a subnode tree holding the recipient table (0x692), the attachment table
//!   (0x671, only with attachments), one PC per attachment and any value over 3580 bytes.
//!
//! Limits: one value is at most 1021 × 8176 bytes (about 8 MB); one object has at most 340
//! subnodes (attachments plus large values); the whole file stays below about 31 MB (no FMap
//! pages). Empty strings and binaries are stored as "no value" (HNID 0), which outlook-pst reads
//! back as `Null` in property contexts; in tables the cell is simply absent. Bulk content
//! (body 0x1000, RTF 0x1009, HTML 0x1013, attachment data 0x3701) is kept out of table rows and
//! lives only in the object's property context.
#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

use std::collections::{BTreeMap, BTreeSet};

/// A MAPI property value in a fixture.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int32(i32),
    Bool(bool),
    /// FILETIME: 100 ns intervals since 1601-01-01 UTC.
    Time(i64),
    Unicode(String),
    Binary(Vec<u8>),
}

/// (property id, value), e.g. (0x0037, Value::Unicode("Subject".into())).
pub type Props = Vec<(u16, Value)>;

#[derive(Debug, Clone, Default)]
pub struct Message {
    pub props: Props,
    pub recipients: Vec<Props>,
    pub attachments: Vec<Props>,
}

#[derive(Debug, Clone, Default)]
pub struct Folder {
    pub name: String,
    /// Extra folder props, e.g. 0x3613 PR_CONTAINER_CLASS.
    pub props: Props,
    pub messages: Vec<Message>,
    pub children: Vec<Folder>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FixtureError(pub String);

type Result<T> = std::result::Result<T, FixtureError>;

fn fail<T>(msg: impl Into<String>) -> Result<T> {
    Err(FixtureError(msg.into()))
}

// ---- constants from [MS-PST] ----

/// Largest data size of one block (8192 minus the 16-byte trailer).
const BLOCK_MAX: usize = 8176;
/// Largest heap allocation; bigger values go to a subnode.
const HEAP_ALLOC_MAX: usize = 3580;
/// Most entries in one XBLOCK, and so most blocks in one data tree.
const XBLOCK_MAX: usize = (BLOCK_MAX - 8) / 8;
const PAGE: u64 = 512;
const AMAP_FIRST: u64 = 0x4400;
/// Bytes covered by one AMap page (496 bytes × 8 bits × 64 bytes).
const AMAP_SPAN: u64 = 496 * 8 * 64;
/// The first FMap page would sit in AMap range 128; files stay below it.
const FILE_LIMIT: u64 = AMAP_FIRST + AMAP_SPAN * 128;

const NID_MESSAGE_STORE: u32 = 0x21;
const NID_NAME_TO_ID_MAP: u32 = 0x61;
const NID_ROOT_FOLDER: u32 = 0x122;
const NID_ATTACHMENT_TABLE: u32 = 0x671;
const NID_RECIPIENT_TABLE: u32 = 0x692;

const NID_TYPE_NORMAL_FOLDER: u32 = 0x02;
const NID_TYPE_NORMAL_MESSAGE: u32 = 0x04;
const NID_TYPE_ATTACHMENT: u32 = 0x05;
const NID_TYPE_HIERARCHY_TABLE: u32 = 0x0D;
const NID_TYPE_CONTENTS_TABLE: u32 = 0x0E;
const NID_TYPE_ASSOC_CONTENTS_TABLE: u32 = 0x0F;
const NID_TYPE_LTP: u32 = 0x1F;
const NID_INDEX_MAX: u32 = (1 << 27) - 1;

const PT_INT32: u16 = 0x0003;
const PT_BOOL: u16 = 0x000B;
const PT_TIME: u16 = 0x0040;
const PT_UNICODE: u16 = 0x001F;
const PT_BINARY: u16 = 0x0102;

const HN_SIG_TC: u8 = 0x7C;
const HN_SIG_BTH: u8 = 0xB5;
const HN_SIG_PC: u8 = 0xBC;

const PTYPE_BBT: u8 = 0x80;
const PTYPE_NBT: u8 = 0x81;
const PTYPE_PMAP: u8 = 0x83;
const PTYPE_AMAP: u8 = 0x84;

/// Row id / row version columns every table carries.
const LTP_ROW_ID: u16 = 0x67F2;
const LTP_ROW_VER: u16 = 0x67F3;
/// Props kept out of table rows (they live only in the object's property context).
const BULK_PROPS: [u16; 4] = [0x1000, 0x1009, 0x1013, 0x3701];

const IPM_SUBTREE_NAME: &str = "Top of Personal Folders";
const WASTEBASKET_NAME: &str = "Deleted Items";
/// Synthetic store record key (PidTagRecordKey).
const RECORD_KEY: [u8; 16] = *b"rslook-fixture01";

fn nid(nid_type: u32, index: u32) -> Result<u32> {
    if index == 0 || index > NID_INDEX_MAX {
        return fail(format!("node index {index:#x} out of range"));
    }
    Ok((index << 5) | nid_type)
}

/// CRC from [MS-PST] 5.3 (reflected polynomial 0xEDB88320, initial value 0, no final XOR).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0_u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    crc
}

/// Block/page signature from [MS-PST] 5.5.
fn signature(ib: u64, bid: u64) -> u16 {
    let value = (ib as u32) ^ (bid as u32);
    ((value >> 16) as u16) ^ (value as u16)
}

fn align_up(value: u64, align: u64) -> u64 {
    value.div_ceil(align).saturating_mul(align)
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn to_u16(value: usize, what: &str) -> Result<u16> {
    u16::try_from(value).map_err(|_| FixtureError(format!("{what} too large: {value}")))
}

fn to_u32(value: usize, what: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| FixtureError(format!("{what} too large: {value}")))
}

/// Little-endian byte buffer.
#[derive(Default)]
struct Buf(Vec<u8>);

impl Buf {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }
}

/// Copies `src` into `dst` at `at`, failing instead of panicking when out of range.
fn put(dst: &mut [u8], at: usize, src: &[u8]) -> Result<()> {
    let end = at.checked_add(src.len()).ok_or_else(|| FixtureError("offset overflow".into()))?;
    match dst.get_mut(at..end) {
        Some(slot) => {
            slot.copy_from_slice(src);
            Ok(())
        }
        None => fail(format!("write at {at}..{end} out of range")),
    }
}

// ---- NDB layer: blocks, data trees, subnode trees, nodes ----

struct RawBlock {
    bid: u64,
    data: Vec<u8>,
}

struct NodeRec {
    nid: u32,
    data: u64,
    sub: u64,
    parent: u32,
}

#[derive(Clone, Copy)]
struct SubNode {
    nid: u32,
    data: u64,
    sub: u64,
}

#[derive(Default)]
struct Ndb {
    blocks: Vec<RawBlock>,
    nodes: Vec<NodeRec>,
    last_bid_index: u64,
}

impl Ndb {
    fn block(&mut self, data: Vec<u8>, internal: bool) -> Result<u64> {
        if data.is_empty() || data.len() > BLOCK_MAX {
            return fail(format!("block size {} out of range", data.len()));
        }
        self.last_bid_index += 1;
        let bid = (self.last_bid_index << 2) | if internal { 2 } else { 0 };
        self.blocks.push(RawBlock { bid, data });
        Ok(bid)
    }

    /// Stores `chunks` (each at most one block) as a data tree: one block, or an XBLOCK over them.
    fn data_tree(&mut self, chunks: Vec<Vec<u8>>) -> Result<u64> {
        if chunks.len() <= 1 {
            return match chunks.into_iter().next() {
                Some(chunk) => self.block(chunk, false),
                None => fail("empty data tree"),
            };
        }
        if chunks.len() > XBLOCK_MAX {
            return fail("value too large for one XBLOCK");
        }
        let total = to_u32(chunks.iter().map(Vec::len).sum(), "data tree")?;
        let mut x = Buf::default();
        x.u8(0x01).u8(1).u16(to_u16(chunks.len(), "data tree entries")?).u32(total);
        for chunk in chunks {
            let bid = self.block(chunk, false)?;
            x.u64(bid);
        }
        self.block(x.0, true)
    }

    /// Writes an SLBLOCK for `subs` (0 when there are none).
    fn subnodes(&mut self, mut subs: Vec<SubNode>) -> Result<u64> {
        if subs.is_empty() {
            return Ok(0);
        }
        if subs.len() > (BLOCK_MAX - 8) / 24 {
            return fail("too many subnodes for one SLBLOCK");
        }
        subs.sort_by_key(|s| s.nid);
        if subs.windows(2).any(|w| matches!(w, [a, b] if a.nid == b.nid)) {
            return fail("duplicate subnode id");
        }
        let mut b = Buf::default();
        b.u8(0x02).u8(0).u16(to_u16(subs.len(), "subnodes")?).u32(0);
        for s in &subs {
            b.u64(u64::from(s.nid)).u64(s.data).u64(s.sub);
        }
        self.block(b.0, true)
    }

    fn node(&mut self, nid: u32, (data, sub): (u64, u64), parent: u32) {
        self.nodes.push(NodeRec { nid, data, sub, parent });
    }
}

// ---- LTP layer: heap-on-node, BTree-on-heap, PC, TC ----

/// Heap-on-node under construction: allocations grouped per data block ("page").
struct Heap {
    client_sig: u8,
    pages: Vec<Vec<Vec<u8>>>,
    user_root: u32,
}

fn heap_page_header_len(page: usize) -> usize {
    match page {
        0 => 12,                 // HNHDR
        p if p % 128 == 8 => 66, // HNBITMAPHDR
        _ => 2,                  // HNPAGEHDR
    }
}

fn heap_page_len(page: usize, allocs: &[Vec<u8>]) -> usize {
    // Allocations are packed; one byte may pad the page map to an even offset.
    let data: usize = allocs.iter().map(Vec::len).sum();
    heap_page_header_len(page) + data + 1 + 4 + 2 * (allocs.len() + 1)
}

/// Fill level of a heap block from its free bytes ([MS-PST] 2.3.1.2).
fn fill_level(free: usize) -> u32 {
    const LIMITS: [usize; 15] = [3584, 2560, 2048, 1792, 1536, 1280, 1024, 768, 512, 256, 128, 64, 32, 16, 8];
    LIMITS.iter().position(|&limit| free >= limit).map_or(15, |p| p as u32)
}

impl Heap {
    fn new(client_sig: u8) -> Self {
        Self { client_sig, pages: Vec::new(), user_root: 0 }
    }

    /// Allocates `data` and returns its HID.
    fn alloc(&mut self, data: Vec<u8>) -> Result<u32> {
        if data.is_empty() || data.len() > HEAP_ALLOC_MAX {
            return fail(format!("heap allocation of {} bytes", data.len()));
        }
        let page_count = self.pages.len();
        let needs_page = match self.pages.last() {
            None => true,
            Some(allocs) => allocs.len() >= 2047 || heap_page_len(page_count - 1, allocs) + data.len() + 4 > BLOCK_MAX,
        };
        if needs_page {
            self.pages.push(Vec::new());
        }
        let block = self.pages.len() - 1;
        if block >= XBLOCK_MAX {
            return fail("heap too large");
        }
        let allocs = self.pages.last_mut().ok_or_else(|| FixtureError("heap has no page".into()))?;
        allocs.push(data);
        Ok((to_u32(block, "heap block")? << 16) | (to_u32(allocs.len(), "heap index")? << 5))
    }

    /// Serialises the heap into one data block per page.
    fn finish(self) -> Result<Vec<Vec<u8>>> {
        let free: Vec<usize> = self.pages.iter().enumerate().map(|(i, a)| BLOCK_MAX.saturating_sub(heap_page_len(i, a))).collect();
        // Fill levels packed 8 per u32, first block in the low nibble.
        let packed = |from: usize, count: usize| -> Vec<u32> {
            (0..count / 8)
                .map(|word| {
                    (0..8).fold(0_u32, |acc, nib| {
                        let level = free.get(from + word * 8 + nib).map_or(0, |&f| fill_level(f));
                        acc | (level << (4 * nib))
                    })
                })
                .collect()
        };
        let mut out = Vec::with_capacity(self.pages.len());
        for (i, allocs) in self.pages.iter().enumerate() {
            let header_len = heap_page_header_len(i);
            let mut body = Buf::default();
            let mut offsets = Vec::with_capacity(allocs.len() + 1);
            for a in allocs {
                offsets.push(to_u16(header_len + body.0.len(), "heap offset")?);
                body.bytes(a);
            }
            offsets.push(to_u16(header_len + body.0.len(), "heap offset")?);
            if !(header_len + body.0.len()).is_multiple_of(2) {
                body.u8(0);
            }
            let map_offset = to_u16(header_len + body.0.len(), "heap map offset")?;
            let mut page = Buf::default();
            page.u16(map_offset);
            match header_len {
                12 => {
                    page.u8(0xEC).u8(self.client_sig).u32(self.user_root);
                    for word in packed(0, 8) {
                        page.u32(word);
                    }
                }
                66 => {
                    for word in packed(i, 128) {
                        page.u32(word);
                    }
                }
                _ => {}
            }
            page.bytes(&body.0);
            page.u16(to_u16(allocs.len(), "heap allocations")?).u16(0);
            for offset in offsets {
                page.u16(offset);
            }
            out.push(page.0);
        }
        Ok(out)
    }
}

/// Builds a BTree-on-heap over `records` (key, data) and returns the HID of its BTHHEADER.
fn bth(heap: &mut Heap, key_size: u8, data_size: u8, mut records: Vec<(u32, Vec<u8>)>) -> Result<u32> {
    records.sort_by_key(|r| r.0);
    let key_bytes = |key: u32| key.to_le_bytes().into_iter().take(usize::from(key_size)).collect::<Vec<u8>>();
    let mut level = 0_u8;
    let mut root = 0_u32;
    if !records.is_empty() {
        let per_leaf = HEAP_ALLOC_MAX / usize::from(key_size + data_size);
        let mut nodes = Vec::new();
        for chunk in records.chunks(per_leaf) {
            let mut b = Buf::default();
            for (key, data) in chunk {
                b.bytes(&key_bytes(*key)).bytes(data);
            }
            nodes.push((chunk.first().map_or(0, |r| r.0), heap.alloc(b.0)?));
        }
        let per_node = HEAP_ALLOC_MAX / (usize::from(key_size) + 4);
        while nodes.len() > 1 {
            let mut parents = Vec::new();
            for chunk in nodes.chunks(per_node) {
                let mut b = Buf::default();
                for (key, hid) in chunk {
                    b.bytes(&key_bytes(*key)).u32(*hid);
                }
                parents.push((chunk.first().map_or(0, |n| n.0), heap.alloc(b.0)?));
            }
            nodes = parents;
            level = level.checked_add(1).ok_or_else(|| FixtureError("BTH too deep".into()))?;
        }
        root = nodes.first().map_or(0, |n| n.1);
    }
    let mut header = Buf::default();
    header.u8(HN_SIG_BTH).u8(key_size).u8(data_size).u8(level).u32(root);
    heap.alloc(header.0)
}

fn prop_type(value: &Value) -> u16 {
    match value {
        Value::Int32(_) => PT_INT32,
        Value::Bool(_) => PT_BOOL,
        Value::Time(_) => PT_TIME,
        Value::Unicode(_) => PT_UNICODE,
        Value::Binary(_) => PT_BINARY,
    }
}

/// One LTP object (PC or TC) under construction: its heap plus its own subnodes.
struct Ltp<'a> {
    ndb: &'a mut Ndb,
    heap: Heap,
    subs: Vec<SubNode>,
}

impl<'a> Ltp<'a> {
    fn new(ndb: &'a mut Ndb, client_sig: u8) -> Self {
        Self { ndb, heap: Heap::new(client_sig), subs: Vec::new() }
    }

    /// Stores `chunks` as a data tree in a new subnode of this object; returns the subnode NID.
    fn subnode(&mut self, chunks: Vec<Vec<u8>>) -> Result<u32> {
        let data = self.ndb.data_tree(chunks)?;
        let sub_nid = nid(NID_TYPE_LTP, to_u32(self.subs.len() + 1, "subnodes")?)?;
        self.subs.push(SubNode { nid: sub_nid, data, sub: 0 });
        Ok(sub_nid)
    }

    /// Stores a variable-size value and returns its HNID (0 for an empty value).
    fn variable(&mut self, value: &Value) -> Result<u32> {
        let bytes = match value {
            Value::Unicode(text) => utf16(text),
            Value::Binary(bytes) => bytes.clone(),
            Value::Int32(_) | Value::Bool(_) | Value::Time(_) => return fail("not a variable-size value"),
        };
        if bytes.is_empty() {
            Ok(0)
        } else if bytes.len() <= HEAP_ALLOC_MAX {
            self.heap.alloc(bytes)
        } else if bytes.len() > XBLOCK_MAX * BLOCK_MAX {
            fail(format!("value of {} bytes is too large", bytes.len()))
        } else {
            self.subnode(bytes.chunks(BLOCK_MAX).map(<[u8]>::to_vec).collect())
        }
    }

    /// Writes the heap (and subnode tree, with `extra` subnodes) and returns (data bid, sub bid).
    fn finish(mut self, user_root: u32, extra: Vec<SubNode>) -> Result<(u64, u64)> {
        self.heap.user_root = user_root;
        let pages = self.heap.finish()?;
        let data = self.ndb.data_tree(pages)?;
        self.subs.extend(extra);
        let sub = self.ndb.subnodes(self.subs)?;
        Ok((data, sub))
    }
}

fn sorted_props<'p>(props: impl IntoIterator<Item = &'p (u16, Value)>) -> Result<Vec<(u16, &'p Value)>> {
    let mut map = BTreeMap::new();
    for (id, value) in props {
        if map.insert(*id, value).is_some() {
            return fail(format!("duplicate property {id:#06x}"));
        }
    }
    Ok(map.into_iter().collect())
}

/// Writes a property context; `extra` subnodes join its subnode tree.
fn property_context(ndb: &mut Ndb, props: &[(u16, Value)], extra: Vec<SubNode>) -> Result<(u64, u64)> {
    let mut ltp = Ltp::new(ndb, HN_SIG_PC);
    let mut records = Vec::new();
    for (id, value) in sorted_props(props)? {
        let hnid = match value {
            Value::Int32(v) => *v as u32,
            Value::Bool(v) => u32::from(*v),
            Value::Time(v) => ltp.heap.alloc(v.to_le_bytes().to_vec())?,
            Value::Unicode(_) | Value::Binary(_) => ltp.variable(value)?,
        };
        let mut data = Buf::default();
        data.u16(prop_type(value)).u32(hnid);
        records.push((u32::from(id), data.0));
    }
    let root = bth(&mut ltp.heap, 2, 6, records)?;
    ltp.finish(root, extra)
}

struct Column {
    id: u16,
    ty: u16,
    offset: u16,
    size: u8,
    bit: u8,
}

/// A table row: row id and the props shown in it.
type Row<'a> = (u32, Vec<(u16, &'a Value)>);

/// Writes a table context. The column set is the union of the rows' props plus the row id and
/// row version columns.
fn table_context(ndb: &mut Ndb, rows: &[Row<'_>]) -> Result<(u64, u64)> {
    let mut types: BTreeMap<u16, u16> = BTreeMap::new();
    for (_, props) in rows {
        for (id, value) in props.iter().filter(|(id, _)| !matches!(*id, LTP_ROW_ID | LTP_ROW_VER)) {
            let ty = prop_type(value);
            if *types.entry(*id).or_insert(ty) != ty {
                return fail(format!("property {id:#06x} has two types in one table"));
            }
        }
    }
    // Row layout: dwRowID, row version, 8-byte, 4-byte, then 1-byte values, then the CEB.
    let mut columns = vec![
        Column { id: LTP_ROW_ID, ty: PT_INT32, offset: 0, size: 4, bit: 0 },
        Column { id: LTP_ROW_VER, ty: PT_INT32, offset: 4, size: 4, bit: 1 },
    ];
    let groups: [(u8, &[u16]); 3] = [(8, &[PT_TIME]), (4, &[PT_INT32, PT_UNICODE, PT_BINARY]), (1, &[PT_BOOL])];
    let mut offset = 8_usize;
    let mut end4 = 8;
    for (size, group) in groups {
        for (&id, &ty) in types.iter().filter(|(_, ty)| group.contains(ty)) {
            let bit = u8::try_from(columns.len()).map_err(|_| FixtureError("too many table columns".into()))?;
            columns.push(Column { id, ty, offset: to_u16(offset, "row")?, size, bit });
            offset += usize::from(size);
        }
        if size == 4 {
            end4 = offset;
        }
    }
    let end1 = offset;
    let row_size = end1 + columns.len().div_ceil(8);
    if row_size > BLOCK_MAX {
        return fail("table row too wide");
    }
    let mut ltp = Ltp::new(ndb, HN_SIG_TC);
    let mut matrix = Vec::with_capacity(rows.len() * row_size);
    let mut index = Vec::with_capacity(rows.len());
    let mut seen = BTreeSet::new();
    for (i, (row_id, props)) in rows.iter().enumerate() {
        if !seen.insert(*row_id) {
            return fail(format!("duplicate row id {row_id:#x}"));
        }
        let mut row = vec![0_u8; row_size];
        put(&mut row, 0, &row_id.to_le_bytes())?;
        let mut bits = vec![0_u8; columns.len().div_ceil(8)];
        for column in &columns {
            let value = if column.bit < 2 {
                None // row id and version: always present
            } else {
                match props.iter().find(|(id, _)| *id == column.id) {
                    Some((_, value)) => Some(*value),
                    None => continue,
                }
            };
            let at = usize::from(column.offset);
            match value {
                None => {}
                Some(Value::Int32(v)) => put(&mut row, at, &v.to_le_bytes())?,
                Some(Value::Bool(v)) => put(&mut row, at, &[u8::from(*v)])?,
                Some(Value::Time(v)) => put(&mut row, at, &v.to_le_bytes())?,
                Some(value) => {
                    let hnid = ltp.variable(value)?;
                    if hnid == 0 {
                        continue; // empty value: leave the cell absent
                    }
                    put(&mut row, at, &hnid.to_le_bytes())?;
                }
            }
            let bit = usize::from(column.bit);
            if let Some(byte) = bits.get_mut(bit / 8) {
                *byte |= 0x80 >> (bit % 8);
            }
        }
        put(&mut row, end1, &bits)?;
        matrix.extend_from_slice(&row);
        index.push((*row_id, to_u32(i, "row index")?.to_le_bytes().to_vec()));
    }

    let rows_hnid = if matrix.is_empty() {
        0
    } else if matrix.len() <= HEAP_ALLOC_MAX {
        ltp.heap.alloc(matrix)?
    } else {
        // Rows never span blocks: each block holds whole rows only.
        let per_block = (BLOCK_MAX / row_size) * row_size;
        if matrix.len().div_ceil(per_block) > XBLOCK_MAX {
            return fail("table too large");
        }
        ltp.subnode(matrix.chunks(per_block).map(<[u8]>::to_vec).collect())?
    };
    let row_index = bth(&mut ltp.heap, 4, 4, index)?;

    columns.sort_by_key(|c| (c.id, c.ty));
    let mut info = Buf::default();
    info.u8(HN_SIG_TC).u8(u8::try_from(columns.len()).map_err(|_| FixtureError("too many table columns".into()))?);
    info.u16(to_u16(end4, "row")?).u16(to_u16(end4, "row")?).u16(to_u16(end1, "row")?).u16(to_u16(row_size, "row")?);
    info.u32(row_index).u32(rows_hnid).u32(0);
    for c in &columns {
        info.u16(c.ty).u16(c.id).u16(c.offset).u8(c.size).u8(c.bit);
    }
    let root = ltp.heap.alloc(info.0)?;
    ltp.finish(root, Vec::new())
}

fn all_props(props: &[(u16, Value)]) -> Vec<(u16, &Value)> {
    props.iter().map(|(id, v)| (*id, v)).collect()
}

/// Props of `props` that belong in a table row.
fn row_props(props: &[(u16, Value)]) -> Vec<(u16, &Value)> {
    props.iter().filter(|(id, _)| !BULK_PROPS.contains(id)).map(|(id, v)| (*id, v)).collect()
}

// ---- messaging layer ----

struct Writer {
    ndb: Ndb,
    next_folder: u32,
    next_message: u32,
    ipm_nid: u32,
    wastebasket: Option<u32>,
}

impl Writer {
    fn alloc_folder(&mut self) -> Result<u32> {
        let folder_nid = nid(NID_TYPE_NORMAL_FOLDER, self.next_folder)?;
        self.next_folder += 1;
        Ok(folder_nid)
    }

    /// Writes `folder` and its subtree; returns the folder's props for its parent's hierarchy row.
    fn write_tree(&mut self, folder_nid: u32, parent: u32, folder: &Folder) -> Result<Props> {
        let mut children = Vec::with_capacity(folder.children.len());
        for child in &folder.children {
            let child_nid = self.alloc_folder()?;
            if folder_nid == self.ipm_nid && child.name == WASTEBASKET_NAME && self.wastebasket.is_none() {
                self.wastebasket = Some(child_nid);
            }
            children.push((child_nid, self.write_tree(child_nid, folder_nid, child)?));
        }
        self.write_folder(folder_nid, parent, folder, children)
    }

    fn write_folder(&mut self, folder_nid: u32, parent: u32, folder: &Folder, children: Vec<(u32, Props)>) -> Result<Props> {
        let mut contents = Vec::with_capacity(folder.messages.len());
        for message in &folder.messages {
            let message_nid = nid(NID_TYPE_NORMAL_MESSAGE, self.next_message)?;
            self.next_message += 1;
            self.write_message(message_nid, folder_nid, message)?;
            contents.push((message_nid, row_props(&message.props)));
        }
        // Unread = MSGFLAG_READ (0x1) not set in PR_MESSAGE_FLAGS (0x0E07).
        let read = |m: &&Message| m.props.iter().any(|(id, v)| *id == 0x0E07 && matches!(v, Value::Int32(f) if f & 1 == 1));
        let unread = folder.messages.iter().filter(|m| !read(m)).count();
        let computed = [0x3001, 0x3602, 0x3603, 0x360A];
        let mut props: Props = vec![
            (0x3001, Value::Unicode(folder.name.clone())),
            (0x3602, Value::Int32(i32::try_from(folder.messages.len()).map_err(|_| FixtureError("too many messages".into()))?)),
            (0x3603, Value::Int32(i32::try_from(unread).map_err(|_| FixtureError("too many messages".into()))?)),
            (0x360A, Value::Bool(!children.is_empty())),
        ];
        props.extend(folder.props.iter().filter(|(id, _)| !computed.contains(id)).cloned());

        let pc = property_context(&mut self.ndb, &props, Vec::new())?;
        self.ndb.node(folder_nid, pc, parent);
        let index = folder_nid >> 5;
        let hierarchy: Vec<Row<'_>> = children.iter().map(|(id, p)| (*id, all_props(p))).collect();
        let tc = table_context(&mut self.ndb, &hierarchy)?;
        self.ndb.node(nid(NID_TYPE_HIERARCHY_TABLE, index)?, tc, 0);
        let tc = table_context(&mut self.ndb, &contents)?;
        self.ndb.node(nid(NID_TYPE_CONTENTS_TABLE, index)?, tc, 0);
        let tc = table_context(&mut self.ndb, &[])?;
        self.ndb.node(nid(NID_TYPE_ASSOC_CONTENTS_TABLE, index)?, tc, 0);
        Ok(props)
    }

    fn write_message(&mut self, message_nid: u32, folder_nid: u32, message: &Message) -> Result<()> {
        let mut subs = Vec::new();
        let mut attachment_rows = Vec::with_capacity(message.attachments.len());
        for (i, attachment) in message.attachments.iter().enumerate() {
            let attachment_nid = nid(NID_TYPE_ATTACHMENT, to_u32(i + 1, "attachments")?)?;
            let (data, sub) = property_context(&mut self.ndb, attachment, Vec::new())?;
            subs.push(SubNode { nid: attachment_nid, data, sub });
            attachment_rows.push((attachment_nid, row_props(attachment)));
        }
        if !attachment_rows.is_empty() {
            let (data, sub) = table_context(&mut self.ndb, &attachment_rows)?;
            subs.push(SubNode { nid: NID_ATTACHMENT_TABLE, data, sub });
        }
        let recipient_rows =
            message.recipients.iter().enumerate().map(|(i, r)| Ok((to_u32(i, "recipients")?, all_props(r)))).collect::<Result<Vec<Row<'_>>>>()?;
        let (data, sub) = table_context(&mut self.ndb, &recipient_rows)?;
        subs.push(SubNode { nid: NID_RECIPIENT_TABLE, data, sub });
        let pc = property_context(&mut self.ndb, &message.props, subs)?;
        self.ndb.node(message_nid, pc, folder_nid);
        Ok(())
    }
}

/// A store entry ID (rgbFlags 0, store record key, NID).
fn entry_id(node: u32) -> Value {
    let mut b = Buf::default();
    b.u32(0).bytes(&RECORD_KEY).u32(node);
    Value::Binary(b.0)
}

/// A complete Unicode PST (no encryption, bCryptMethod NONE) whose IPM subtree (Top of Personal
/// Folders) holds `folders`.
pub fn write_pst(folders: &[Folder]) -> Result<Vec<u8>> {
    let mut w = Writer { ndb: Ndb::default(), next_folder: 0x400, next_message: 0x10000, ipm_nid: 0, wastebasket: None };
    w.ipm_nid = w.alloc_folder()?;
    let search_nid = w.alloc_folder()?;

    let ipm = Folder { name: IPM_SUBTREE_NAME.into(), children: folders.to_vec(), ..Folder::default() };
    let ipm_row = w.write_tree(w.ipm_nid, NID_ROOT_FOLDER, &ipm)?;
    let search = Folder { name: "Search Root".into(), ..Folder::default() };
    let search_row = w.write_tree(search_nid, NID_ROOT_FOLDER, &search)?;
    let root = Folder { name: "Root".into(), ..Folder::default() };
    let ipm_nid = w.ipm_nid;
    w.write_folder(NID_ROOT_FOLDER, NID_ROOT_FOLDER, &root, vec![(ipm_nid, ipm_row), (search_nid, search_row)])?;

    let mut store: Props = vec![
        (0x0FF9, Value::Binary(RECORD_KEY.to_vec())),
        (0x3001, Value::Unicode("Rslook fixture".into())),
        (0x35E0, entry_id(ipm_nid)),
        (0x35E7, entry_id(search_nid)),
    ];
    if let Some(wastebasket) = w.wastebasket {
        store.push((0x35E3, entry_id(wastebasket)));
    }
    let pc = property_context(&mut w.ndb, &store, Vec::new())?;
    w.ndb.node(NID_MESSAGE_STORE, pc, 0);

    // Empty name-to-ID map: bucket count plus empty GUID, entry and string streams.
    let name_map: Props = vec![
        (0x0001, Value::Int32(251)),
        (0x0002, Value::Binary(Vec::new())),
        (0x0003, Value::Binary(Vec::new())),
        (0x0004, Value::Binary(Vec::new())),
    ];
    let pc = property_context(&mut w.ndb, &name_map, Vec::new())?;
    w.ndb.node(NID_NAME_TO_ID_MAP, pc, 0);

    layout(w.ndb, w.next_folder, w.next_message)
}

// ---- file layout: blocks, B-tree pages, AMap/PMap pages, header ----

/// Hands out file space, skipping the AMap/PMap pages that start each AMap range.
struct Space {
    next: u64,
    used: Vec<(u64, u64)>,
}

impl Space {
    fn place(&mut self, size: u64, align: u64) -> Result<u64> {
        loop {
            let start = align_up(self.next, align);
            let range = (start - AMAP_FIRST) / AMAP_SPAN;
            let range_start = AMAP_FIRST + range * AMAP_SPAN;
            let reserved = range_start + PAGE * if range.is_multiple_of(8) { 2 } else { 1 };
            if start < reserved {
                self.next = reserved;
            } else if start + size > range_start + AMAP_SPAN {
                self.next = range_start + AMAP_SPAN;
            } else if start + size > FILE_LIMIT {
                return fail("fixture too large (would need FMap pages)");
            } else {
                self.next = start + size;
                self.used.push((start, size));
                return Ok(start);
            }
        }
    }
}

/// Everything placed in the file, plus the page BID counter.
struct Layout {
    space: Space,
    out: Vec<(u64, Vec<u8>)>,
    last_page_index: u64,
}

impl Layout {
    /// Stores a 512-byte page at `ib` whose first 496 bytes are `body`.
    fn store_page(&mut self, ib: u64, bid: u64, ptype: u8, sig: u16, mut body: Vec<u8>) -> Result<()> {
        if body.len() > 496 {
            return fail("page body too large");
        }
        body.resize(496, 0);
        let crc = crc32(&body);
        let mut b = Buf(body);
        b.u8(ptype).u8(ptype).u16(sig).u32(crc).u64(bid);
        self.out.push((ib, b.0));
        Ok(())
    }

    /// Builds a B-tree from sorted (key, entry bytes) and returns the root BREF (bid, ib).
    fn btree(&mut self, ptype: u8, entry_size: u8, entries: Vec<(u64, Vec<u8>)>) -> Result<(u64, u64)> {
        let mut level = 0_u8;
        let mut size = entry_size;
        let mut items = entries;
        loop {
            let max = 488 / usize::from(size);
            let chunks: Vec<&[(u64, Vec<u8>)]> = if items.is_empty() { vec![&[]] } else { items.chunks(max).collect() };
            let mut refs = Vec::with_capacity(chunks.len());
            for chunk in chunks {
                let mut b = Buf::default();
                for (_, entry) in chunk {
                    b.bytes(entry);
                }
                b.0.resize(488, 0);
                b.u8(chunk.len() as u8).u8(max as u8).u8(size).u8(level).u32(0);
                let ib = self.space.place(PAGE, PAGE)?;
                self.last_page_index += 1;
                let bid = self.last_page_index << 2;
                self.store_page(ib, bid, ptype, signature(ib, bid), b.0)?;
                refs.push((chunk.first().map_or(0, |e| e.0), bid, ib));
            }
            if let [(_, bid, ib)] = refs.as_slice() {
                return Ok((*bid, *ib));
            }
            items = refs
                .into_iter()
                .map(|(key, bid, ib)| {
                    let mut b = Buf::default();
                    b.u64(key).u64(bid).u64(ib);
                    (key, b.0)
                })
                .collect();
            level = level.checked_add(1).ok_or_else(|| FixtureError("B-tree too deep".into()))?;
            size = 24;
        }
    }
}

fn layout(ndb: Ndb, next_folder: u32, next_message: u32) -> Result<Vec<u8>> {
    let mut lay = Layout { space: Space { next: AMAP_FIRST, used: Vec::new() }, out: Vec::new(), last_page_index: 0 };

    // Blocks: data, padding, 16-byte trailer, 64-byte aligned.
    let mut bbt = Vec::with_capacity(ndb.blocks.len());
    for block in &ndb.blocks {
        let cb = to_u16(block.data.len(), "block")?;
        let size = align_up(u64::from(cb) + 16, 64);
        let ib = lay.space.place(size, 64)?;
        let mut b = Buf(block.data.clone());
        b.0.resize(usize::from(cb) + (size as usize - 16 - usize::from(cb)), 0);
        b.u16(cb).u16(signature(ib, block.bid)).u32(crc32(&block.data)).u64(block.bid);
        lay.out.push((ib, b.0));
        let mut entry = Buf::default();
        entry.u64(block.bid).u64(ib).u16(cb).u16(1).u32(0);
        bbt.push((block.bid, entry.0));
    }
    bbt.sort_by_key(|e| e.0);

    let mut nodes: Vec<&NodeRec> = ndb.nodes.iter().collect();
    nodes.sort_by_key(|n| n.nid);
    if nodes.windows(2).any(|w| matches!(w, [a, b] if a.nid == b.nid)) {
        return fail("duplicate node id");
    }
    let nbt = nodes
        .iter()
        .map(|n| {
            let mut b = Buf::default();
            b.u64(u64::from(n.nid)).u64(n.data).u64(n.sub).u32(n.parent).u32(0);
            (u64::from(n.nid), b.0)
        })
        .collect();
    let nbt_root = lay.btree(PTYPE_NBT, 32, nbt)?;
    let bbt_root = lay.btree(PTYPE_BBT, 24, bbt)?;

    // One AMap page per range (plus a PMap page every 8 ranges); the file is padded to the end of
    // the last range so every AMap covers bytes that exist.
    let ranges = (lay.space.next - AMAP_FIRST).div_ceil(AMAP_SPAN).max(1);
    let eof = AMAP_FIRST + ranges * AMAP_SPAN;
    let mut maps = vec![vec![0_u8; 496]; usize::try_from(ranges).map_err(|_| FixtureError("file too large".into()))?];
    let mut reserved = Vec::new();
    for r in 0..ranges {
        let start = AMAP_FIRST + r * AMAP_SPAN;
        reserved.push((start, PAGE));
        if r.is_multiple_of(8) {
            reserved.push((start + PAGE, PAGE));
        }
    }
    let mut allocated = 0_u64;
    for &(start, size) in reserved.iter().chain(lay.space.used.iter()) {
        allocated += size;
        let first = (start - AMAP_FIRST) / 64;
        for slot in first..first + size / 64 {
            let range = (slot / (496 * 8)) as usize;
            let bit = (slot % (496 * 8)) as usize;
            if let Some(byte) = maps.get_mut(range).and_then(|m| m.get_mut(bit / 8)) {
                *byte |= 0x80 >> (bit % 8);
            }
        }
    }
    let amap_free = ranges * AMAP_SPAN - allocated;
    for (r, map) in maps.into_iter().enumerate() {
        let start = AMAP_FIRST + r as u64 * AMAP_SPAN;
        lay.store_page(start, start, PTYPE_AMAP, 0, map)?;
        if r.is_multiple_of(8) {
            lay.store_page(start + PAGE, start + PAGE, PTYPE_PMAP, 0, vec![0xFF; 496])?;
        }
    }
    let amap_last = AMAP_FIRST + (ranges - 1) * AMAP_SPAN;

    let mut file = vec![0_u8; usize::try_from(eof).map_err(|_| FixtureError("file too large".into()))?];
    for (ib, bytes) in &lay.out {
        put(&mut file, usize::try_from(*ib).map_err(|_| FixtureError("file too large".into()))?, bytes)?;
    }

    // HEADER ([MS-PST] 2.2.2.6, Unicode): the 516 bytes covered by dwCRCFull.
    let mut rgnid = [0x400_u32; 32];
    for (nid_type, next) in
        [(3, 0x4000), (8, 0x8000), (NID_TYPE_NORMAL_FOLDER as usize, next_folder), (NID_TYPE_NORMAL_MESSAGE as usize, next_message)]
    {
        if let Some(slot) = rgnid.get_mut(nid_type) {
            *slot = next;
        }
    }
    let mut h = Buf::default();
    h.u16(0x4D53).u16(23).u16(19).u8(1).u8(1).u32(0).u32(0).u64(0); // wMagicClient .. bidUnused
    h.u64((lay.last_page_index + 1) << 2).u32(1); // bidNextP, dwUnique
    for n in rgnid {
        h.u32(n);
    }
    h.u64(0); // qwUnused
    h.u32(0).u64(eof).u64(amap_last).u64(amap_free).u64(0); // ROOT: dwReserved .. cbPMapFree
    h.u64(nbt_root.0).u64(nbt_root.1).u64(bbt_root.0).u64(bbt_root.1);
    h.u8(0x02).u8(0).u16(0); // fAMapValid = VALID_AMAP2
    h.u32(0).bytes(&[0xFF; 128]).bytes(&[0xFF; 128]).u8(0x80).u8(0).u16(0); // dwAlign .. rgbReserved
    h.u64((ndb.last_bid_index + 1) << 2); // bidNextB
    if h.0.len() != 516 {
        return fail("internal error: header size");
    }
    let partial = crc32(h.0.get(..471).unwrap_or_default());
    let full = crc32(&h.0);
    let mut header = Buf::default();
    header.bytes(b"!BDN").u32(partial).bytes(&h.0).u32(full).bytes(&[0; 36]);
    put(&mut file, 0, &header.0)?;
    Ok(file)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use outlook_pst::ltp::prop_context::PropertyValue;
    use outlook_pst::ltp::table_context::TableContext;
    use outlook_pst::messaging::attachment::{Attachment as _, UnicodeAttachment};
    use outlook_pst::messaging::message::UnicodeMessage;
    use outlook_pst::messaging::store::{EntryId, Store, UnicodeStore};
    use outlook_pst::ndb::node_id::NodeId;
    use std::rc::Rc;

    fn s(text: &str) -> Value {
        Value::Unicode(text.into())
    }

    fn from_pst(value: &PropertyValue) -> Option<Value> {
        match value {
            PropertyValue::Integer32(v) => Some(Value::Int32(*v)),
            PropertyValue::Boolean(v) => Some(Value::Bool(*v)),
            PropertyValue::Time(v) => Some(Value::Time(*v)),
            PropertyValue::Unicode(v) => Some(Value::Unicode(v.to_string())),
            PropertyValue::Binary(v) => Some(Value::Binary(v.buffer().to_vec())),
            _ => None,
        }
    }

    fn message(i: usize, body: String) -> Message {
        Message {
            props: vec![
                (0x001A, s("IPM.Note")),
                (0x0037, s(&format!("Subject {i} – ünïcödé ✉"))),
                (0x1000, s(&body)),
                (0x0E07, Value::Int32(if i.is_multiple_of(2) { 1 } else { 0 })),
                (0x0E06, Value::Time(133_000_000_000_000_000 + i as i64)),
                (0x0C1A, s("Alice Sender")),
                (0x0C1F, s("alice@example.test")),
                (0x0017, Value::Int32(2)),
                (0x1090, Value::Int32(2)),
                (0x0E1F, Value::Bool(i.is_multiple_of(3))),
                (0x1013, Value::Binary(format!("<p>message {i}</p>").into_bytes())),
            ],
            ..Message::default()
        }
    }

    fn sample() -> Vec<Folder> {
        let big_body: String = "Lorem ipsum dolor sit amet. ".repeat(300).chars().take(8000).collect();
        let attachment_data: Vec<u8> = (0..8000_u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut rich = message(0, big_body);
        rich.recipients = vec![
            vec![(0x0C15, Value::Int32(1)), (0x3001, s("Bob")), (0x3003, s("bob@example.test")), (0x39FE, s("bob@example.test"))],
            vec![(0x0C15, Value::Int32(2)), (0x3001, s("Carol")), (0x3003, s("carol@example.test"))],
        ];
        rich.attachments = vec![
            vec![
                (0x3707, s("report.bin")),
                (0x3704, s("report.bin")),
                (0x0E20, Value::Int32(8000)),
                (0x3701, Value::Binary(attachment_data)),
                (0x3705, Value::Int32(1)),
            ],
            vec![
                (0x3707, s("note.txt")),
                (0x3704, s("note.txt")),
                (0x0E20, Value::Int32(5)),
                (0x3701, Value::Binary(b"hello".to_vec())),
                (0x3705, Value::Int32(1)),
            ],
        ];
        let bulk: Vec<Message> = (0..150).map(|i| message(i, format!("Body of message {i}"))).collect();
        vec![
            Folder {
                name: "Inbox".into(),
                messages: vec![rich, message(1, "second".into())],
                children: vec![Folder {
                    name: "Level 2".into(),
                    children: vec![Folder {
                        name: "Level 3".into(),
                        children: vec![Folder { name: "Level 4".into(), ..Folder::default() }],
                        messages: vec![message(7, "deep".into())],
                        ..Folder::default()
                    }],
                    ..Folder::default()
                }],
                ..Folder::default()
            },
            Folder { name: "Bulk".into(), messages: bulk, ..Folder::default() },
            Folder { name: "Calendar".into(), props: vec![(0x3613, s("IPF.Appointment"))], ..Folder::default() },
            Folder { name: WASTEBASKET_NAME.into(), ..Folder::default() },
        ]
    }

    /// A temp file deleted on drop.
    struct TempFile(std::path::PathBuf);

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn temp_pst(bytes: &[u8], tag: &str) -> TempFile {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("rslook-fixture-{tag}-{}-{nanos}.pst", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        TempFile(path)
    }

    /// Reads all rows of a table as (row id, prop id -> value).
    fn table_rows(table: &dyn TableContext) -> Vec<(u32, BTreeMap<u16, Value>)> {
        let context = table.context();
        table
            .rows_matrix()
            .map(|row| {
                let mut values = BTreeMap::new();
                for (column, cell) in context.columns().iter().zip(row.columns(context).unwrap()) {
                    if let Some(cell) = cell {
                        let value = table.read_column(&cell, column.prop_type()).unwrap();
                        if let Some(value) = from_pst(&value) {
                            values.insert(column.prop_id(), value);
                        }
                    }
                }
                (u32::from(row.id()), values)
            })
            .collect()
    }

    fn assert_props(expected: &[(u16, Value)], get: impl Fn(u16) -> Option<Value>, what: &str) {
        for (id, value) in expected {
            assert_eq!(get(*id).as_ref(), Some(value), "{what}: property {id:#06x}");
        }
    }

    /// Walks a folder the way the importer does and checks it against `expected`.
    fn check_folder(store: &Rc<dyn Store>, entry: &EntryId, expected: &Folder, pst: &Rc<UnicodeStore>, seen: &mut usize) {
        let folder = store.open_folder(entry).unwrap();
        let props = folder.properties();
        assert_eq!(props.display_name().unwrap(), expected.name);
        assert_eq!(props.content_count().unwrap(), expected.messages.len() as i32);
        let unread = expected.messages.iter().filter(|m| !m.props.contains(&(0x0E07, Value::Int32(1)))).count();
        assert_eq!(props.unread_count().unwrap(), unread as i32);
        assert_eq!(props.get(0x360A).and_then(from_pst), Some(Value::Bool(!expected.children.is_empty())));
        assert_props(&expected.props, |id| props.get(id).and_then(from_pst), &expected.name);
        *seen += 1;

        let rows = table_rows(folder.hierarchy_table().expect("hierarchy table").as_ref());
        assert_eq!(rows.len(), expected.children.len());
        for ((row_id, values), child) in rows.iter().zip(&expected.children) {
            assert_eq!(values.get(&0x3001), Some(&Value::Unicode(child.name.clone())));
            let child_entry = store.properties().make_entry_id(NodeId::from(*row_id)).unwrap();
            check_folder(store, &child_entry, child, pst, seen);
        }

        let rows = table_rows(folder.contents_table().expect("contents table").as_ref());
        assert_eq!(rows.len(), expected.messages.len());
        for ((row_id, values), want) in rows.iter().zip(&expected.messages) {
            assert_props(
                &want.props.iter().filter(|p| !BULK_PROPS.contains(&p.0)).cloned().collect::<Vec<_>>(),
                |id| values.get(&id).cloned(),
                "contents row",
            );
            assert!(!values.contains_key(&0x1000), "body must stay out of the contents table");
            let entry_id = store.properties().make_entry_id(NodeId::from(*row_id)).unwrap();
            let message = store.open_message(&entry_id, None).unwrap();
            assert_props(&want.props, |id| message.properties().get(id).and_then(from_pst), "message");

            let recipients = table_rows(message.recipient_table().expect("recipient table").as_ref());
            assert_eq!(recipients.len(), want.recipients.len());
            for ((_, got), want) in recipients.iter().zip(&want.recipients) {
                assert_props(want, |id| got.get(&id).cloned(), "recipient");
            }

            match message.attachment_table() {
                None => assert!(want.attachments.is_empty()),
                Some(table) => {
                    let rows = table_rows(table.as_ref());
                    assert_eq!(rows.len(), want.attachments.len());
                    let full = UnicodeMessage::read(pst.clone(), &entry_id, None).unwrap();
                    for ((row_id, got), want) in rows.iter().zip(&want.attachments) {
                        let in_table: Vec<(u16, Value)> = want.iter().filter(|p| p.0 != 0x3701).cloned().collect();
                        assert_props(&in_table, |id| got.get(&id).cloned(), "attachment row");
                        assert!(!got.contains_key(&0x3701));
                        let attachment = UnicodeAttachment::read(full.clone(), NodeId::from(*row_id), None).unwrap();
                        assert_props(want, |id| attachment.properties().get(id).and_then(from_pst), "attachment PC");
                    }
                }
            }
        }
    }

    #[test]
    fn written_pst_round_trips_through_outlook_pst() {
        let folders = sample();
        let bytes = write_pst(&folders).unwrap();
        let file = temp_pst(&bytes, "roundtrip");

        let store = outlook_pst::open_store(&file.0).unwrap();
        assert_eq!(store.properties().display_name().unwrap(), "Rslook fixture");
        let pst = UnicodeStore::read(Rc::new(outlook_pst::UnicodePstFile::open(&file.0).unwrap())).unwrap();

        let ipm_entry = store.properties().ipm_sub_tree_entry_id().unwrap();
        let ipm = Folder { name: IPM_SUBTREE_NAME.into(), children: folders, ..Folder::default() };
        let mut seen = 0;
        check_folder(&store, &ipm_entry, &ipm, &pst, &mut seen);
        assert_eq!(seen, 8, "IPM subtree plus the 7 caller folders");

        let wastebasket = store.open_folder(&store.properties().ipm_wastebasket_entry_id().unwrap()).unwrap();
        assert_eq!(wastebasket.properties().display_name().unwrap(), WASTEBASKET_NAME);
        let finder = store.open_folder(&store.properties().finder_entry_id().unwrap()).unwrap();
        assert_eq!(finder.properties().display_name().unwrap(), "Search Root");
        // (Store::root_hierarchy_table deadlocks in outlook-pst 1.2.0: it locks the file reader twice.)
        let root = store.open_folder(&store.properties().make_entry_id(NodeId::from(NID_ROOT_FOLDER)).unwrap()).unwrap();
        let names: Vec<_> = table_rows(root.hierarchy_table().unwrap().as_ref()).into_iter().map(|(_, v)| v.get(&0x3001).cloned()).collect();
        assert_eq!(names, [Some(s(IPM_SUBTREE_NAME)), Some(s("Search Root"))]);
        assert!(store.named_property_map().is_ok());
    }

    /// 500 rows with long subjects: the contents table needs a heap of more than 8 blocks (so an
    /// HNBITMAPHDR block) and a two-level row index BTH.
    #[test]
    fn large_contents_table_reads_back() {
        let messages: Vec<Message> =
            (0..500).map(|i| Message { props: vec![(0x0037, s(&format!("{i:04} {}", "x".repeat(300))))], ..Message::default() }).collect();
        let bytes = write_pst(&[Folder { name: "Big".into(), messages, ..Folder::default() }]).unwrap();
        let file = temp_pst(&bytes, "large");
        let store = outlook_pst::open_store(&file.0).unwrap();
        let ipm = store.open_folder(&store.properties().ipm_sub_tree_entry_id().unwrap()).unwrap();
        let (big_id, _) = table_rows(ipm.hierarchy_table().unwrap().as_ref()).remove(0);
        let big = store.open_folder(&store.properties().make_entry_id(NodeId::from(big_id)).unwrap()).unwrap();
        let rows = table_rows(big.contents_table().unwrap().as_ref());
        assert_eq!(rows.len(), 500);
        for (i, (row_id, values)) in rows.iter().enumerate() {
            let want = s(&format!("{i:04} {}", "x".repeat(300)));
            assert_eq!(values.get(&0x0037), Some(&want));
            let entry = store.properties().make_entry_id(NodeId::from(*row_id)).unwrap();
            assert_eq!(store.open_message(&entry, None).unwrap().properties().get(0x0037).and_then(from_pst), Some(want));
        }
    }

    #[test]
    fn empty_store_opens() {
        let bytes = write_pst(&[]).unwrap();
        let file = temp_pst(&bytes, "empty");
        let store = outlook_pst::open_store(&file.0).unwrap();
        let ipm = store.open_folder(&store.properties().ipm_sub_tree_entry_id().unwrap()).unwrap();
        assert_eq!(ipm.properties().display_name().unwrap(), IPM_SUBTREE_NAME);
        assert!(table_rows(ipm.hierarchy_table().unwrap().as_ref()).is_empty());
        assert!(table_rows(ipm.contents_table().unwrap().as_ref()).is_empty());
        assert!(store.properties().ipm_wastebasket_entry_id().is_err());
    }

    #[test]
    fn crc_and_signature_match_spec() {
        // [MS-PST] CRC is CRC-32 without the initial/final inversion: undoing that inversion
        // here must give the well-known CRC-32 check value.
        let mut crc = 0xFFFF_FFFF_u32;
        for &byte in b"123456789" {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        assert_eq!(!crc, 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_ne!(crc32(b"123456789"), 0);
        assert_eq!(signature(0x1234_5678, 0x4), 0x1234 ^ 0x567C);
    }

    #[test]
    fn rejects_duplicate_props_and_oversized_values() {
        let dup = Folder { name: "x".into(), props: vec![(0x3613, s("a")), (0x3613, s("b"))], ..Folder::default() };
        assert!(write_pst(&[dup]).is_err());
        let huge = Message { props: vec![(0x1013, Value::Binary(vec![1; 9_000_000]))], ..Message::default() };
        assert!(write_pst(&[Folder { name: "x".into(), messages: vec![huge], ..Folder::default() }]).is_err());
    }
}
