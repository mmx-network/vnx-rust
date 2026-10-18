//! Validated VNX byte code. All offsets are relative to their container.
use crate::{error::invalid, Result};
use std::{collections::BTreeMap, mem::size_of};

type SizeCache = BTreeMap<(u64, usize), Option<usize>>;
pub const CODE_NULL: u16 = 0;
pub const CODE_UINT8: u16 = 1;
pub const CODE_UINT16: u16 = 2;
pub const CODE_UINT32: u16 = 3;
pub const CODE_UINT64: u16 = 4;
pub const CODE_INT8: u16 = 5;
pub const CODE_INT16: u16 = 6;
pub const CODE_INT32: u16 = 7;
pub const CODE_INT64: u16 = 8;
pub const CODE_FLOAT: u16 = 9;
pub const CODE_DOUBLE: u16 = 10;
pub const CODE_ARRAY: u16 = 11;
pub const CODE_LIST: u16 = 12;
pub const CODE_MAP: u16 = 13;
pub const CODE_TYPE_CODE: u16 = 14;
pub const CODE_TYPE: u16 = 15;
pub const CODE_ANY: u16 = 16;
pub const CODE_DYNAMIC: u16 = 17;
pub const CODE_PADDING: u16 = 18;
pub const CODE_STRUCT: u16 = 19;
pub const CODE_MATRIX: u16 = 21;
pub const CODE_IMAGE: u16 = 22;
pub const CODE_TUPLE: u16 = 23;
pub const CODE_OBJECT: u16 = 24;
pub const CODE_BOOL: u16 = 31;
pub const CODE_STRING: u16 = 32;
pub const CODE_OPTIONAL: u16 = 33;
pub const CODE_MAGIC: u16 = 0x3713;
pub const CODE_NONE: u16 = 0xffff;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Code {
    /// This node's tag and header parameters use VNX's alternate byte order.
    /// Children retain their own independent byte-order tags.
    Alt(Box<Code>),
    Null,
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
    Bool,
    String,
    Array(usize, Box<Code>),
    List(Box<Code>),
    Map(Box<Code>, Box<Code>),
    Tuple(Vec<Code>),
    Optional(Box<Code>),
    Struct(usize),
    Matrix(Vec<usize>, Box<Code>),
    Image(usize, Box<Code>),
    Any,
    Dynamic,
    Object,
    TypeCode,
}
impl Code {
    pub fn from_words(words: &[u16]) -> Result<Self> {
        let (value, n) = Self::parse(words, 0)?;
        if n != words.len() {
            return Err(invalid("trailing byte code"));
        }
        Ok(value)
    }
    fn parse(w: &[u16], depth: usize) -> Result<(Self, usize)> {
        if depth >= 100 {
            return Err(invalid("byte code recursion"));
        }
        let raw = *w.first().ok_or_else(|| invalid("empty byte code"))?;
        let alternate = raw > 255 && raw & 255 == 0;
        let first = if alternate { raw.swap_bytes() } else { raw };
        let (value, consumed) = Self::parse_node(w, depth, first, alternate)?;
        Ok((
            if alternate {
                Self::Alt(Box::new(value))
            } else {
                value
            },
            consumed,
        ))
    }
    fn parse_node(w: &[u16], depth: usize, first: u16, alternate: bool) -> Result<(Self, usize)> {
        let parameter = |index: usize, message: &'static str| -> Result<usize> {
            let value = *w.get(index).ok_or_else(|| invalid(message))?;
            Ok(if alternate { value.swap_bytes() } else { value } as usize)
        };
        let atom = match first {
            0 => Self::Null,
            1 => Self::U8,
            2 => Self::U16,
            3 => Self::U32,
            4 => Self::U64,
            5 => Self::I8,
            6 => Self::I16,
            7 => Self::I32,
            8 => Self::I64,
            9 => Self::F32,
            10 => Self::F64,
            14 => Self::TypeCode,
            16 => Self::Any,
            17 => Self::Dynamic,
            24 => Self::Object,
            31 => Self::Bool,
            32 => Self::String,
            11 | 12 | 33 => {
                let offset = if first == 11 { 2 } else { 1 };
                let sub = w
                    .get(offset..)
                    .ok_or_else(|| invalid("short container code"))?;
                let (code, n) = Self::parse(sub, depth + 1)?;
                let out = match first {
                    11 => Self::Array(parameter(1, "short array")?, Box::new(code)),
                    12 => Self::List(Box::new(code)),
                    _ => Self::Optional(Box::new(code)),
                };
                return Ok((out, n + offset));
            }
            13 => {
                let split = parameter(1, "short map")?;
                if split < 3 || split >= w.len() {
                    return Err(invalid("map value offset"));
                }
                let (key, n) = Self::parse(&w[2..split], depth + 1)?;
                if n + 2 != split {
                    return Err(invalid("map key offset"));
                }
                let (value, n) = Self::parse(&w[split..], depth + 1)?;
                return Ok((Self::Map(Box::new(key), Box::new(value)), split + n));
            }
            19 => return Ok((Self::Struct(parameter(1, "short struct")?), 2)),
            21 | 22 => {
                let dim = parameter(1, "short dimensions")?;
                if dim == 0 || dim > 32 {
                    return Err(invalid("invalid dimensions"));
                }
                let offset = if first == 21 { 2 + dim } else { 2 };
                let (code, n) = Self::parse(
                    w.get(offset..).ok_or_else(|| invalid("short dimensions"))?,
                    depth + 1,
                )?;
                let out = if first == 21 {
                    Self::Matrix(
                        (2..offset)
                            .map(|index| parameter(index, "short matrix"))
                            .collect::<Result<Vec<_>>>()?,
                        Box::new(code),
                    )
                } else {
                    Self::Image(dim, Box::new(code))
                };
                return Ok((out, offset + n));
            }
            23 => {
                let count = parameter(1, "short tuple")?;
                if count > 32 || w.len() < 2 + count {
                    return Err(invalid("tuple size"));
                }
                let mut pos = 2 + count;
                let mut parts = Vec::new();
                for index in 2..2 + count {
                    if parameter(index, "short tuple offset")? != pos {
                        return Err(invalid("tuple offset"));
                    }
                    let (code, n) = Self::parse(&w[pos..], depth + 1)?;
                    parts.push(code);
                    pos += n;
                }
                return Ok((Self::Tuple(parts), pos));
            }
            _ => return Err(invalid(format!("unknown byte code {first}"))),
        };
        Ok((atom, 1))
    }
    pub fn to_words(&self) -> Vec<u16> {
        match self {
            Self::Alt(code) => {
                let mut words = code.to_words();
                let count = match code.as_ref() {
                    Self::Array(_, _) | Self::Map(_, _) | Self::Struct(_) | Self::Image(_, _) => 2,
                    Self::Tuple(parts) => 2 + parts.len(),
                    Self::Matrix(dimensions, _) => 2 + dimensions.len(),
                    // An alternate wrapper changes one node, never its children.
                    _ => 1,
                };
                for word in &mut words[..count] {
                    *word = word.swap_bytes();
                }
                words
            }
            Self::Null => vec![0],
            Self::U8 => vec![1],
            Self::U16 => vec![2],
            Self::U32 => vec![3],
            Self::U64 => vec![4],
            Self::I8 => vec![5],
            Self::I16 => vec![6],
            Self::I32 => vec![7],
            Self::I64 => vec![8],
            Self::F32 => vec![9],
            Self::F64 => vec![10],
            Self::Bool => vec![31],
            Self::String => vec![32],
            Self::Any => vec![16],
            Self::Dynamic => vec![17],
            Self::Object => vec![24],
            Self::TypeCode => vec![14],
            Self::Array(n, c) => {
                let mut w = vec![11, *n as u16];
                w.extend(c.to_words());
                w
            }
            Self::List(c) | Self::Optional(c) => {
                let mut w = vec![if matches!(self, Self::List(_)) {
                    12
                } else {
                    33
                }];
                w.extend(c.to_words());
                w
            }
            Self::Map(k, v) => {
                let k = k.to_words();
                let mut w = vec![13, (2 + k.len()) as u16];
                w.extend(k);
                w.extend(v.to_words());
                w
            }
            Self::Struct(i) => vec![19, *i as u16],
            Self::Tuple(parts) => {
                let mut w = vec![23, parts.len() as u16];
                w.resize(2 + parts.len(), 0);
                for (i, c) in parts.iter().enumerate() {
                    w[2 + i] = w.len() as u16;
                    w.extend(c.to_words());
                }
                w
            }
            Self::Matrix(d, c) => {
                let mut w = vec![21, d.len() as u16];
                w.extend(d.iter().map(|x| *x as u16));
                w.extend(c.to_words());
                w
            }
            Self::Image(d, c) => {
                let mut w = vec![22, *d as u16];
                w.extend(c.to_words());
                w
            }
        }
    }
    pub fn fixed_size(
        &self,
        registry: &crate::Registry,
        owner: Option<&crate::TypeCode>,
    ) -> Result<Option<usize>> {
        self.fixed_size_bounded(registry, owner, &mut |_| Ok(()))
    }
    pub(crate) fn fixed_size_bounded(
        &self,
        registry: &crate::Registry,
        owner: Option<&crate::TypeCode>,
        allocate: &mut impl FnMut(usize) -> Result<()>,
    ) -> Result<Option<usize>> {
        self.size(registry, owner, 0, &mut SizeCache::new(), allocate)
    }
    fn size(
        &self,
        r: &crate::Registry,
        o: Option<&crate::TypeCode>,
        depth: usize,
        cache: &mut SizeCache,
        allocate: &mut impl FnMut(usize) -> Result<()>,
    ) -> Result<Option<usize>> {
        if depth >= 100 {
            return Err(invalid("recursive inline struct"));
        }
        let mul = |a: usize, b: usize| {
            a.checked_mul(b)
                .ok_or_else(|| invalid("fixed size overflow"))
        };
        Ok(match self {
            Self::Alt(code) => code.size(r, o, depth + 1, cache, allocate)?,
            Self::Null => Some(0),
            Self::U8 | Self::I8 | Self::Bool => Some(1),
            Self::U16 | Self::I16 => Some(2),
            Self::U32 | Self::I32 | Self::F32 => Some(4),
            Self::U64 | Self::I64 | Self::F64 => Some(8),
            Self::Array(n, c) => c
                .size(r, o, depth + 1, cache, allocate)?
                .map(|s| mul(*n, s))
                .transpose()?,
            Self::Matrix(d, c) => {
                let n = d.iter().try_fold(1, |n, d| mul(n, *d))?;
                c.size(r, o, depth + 1, cache, allocate)?
                    .map(|s| mul(n, s))
                    .transpose()?
            }
            Self::Tuple(parts) => {
                let mut n = 0usize;
                for c in parts {
                    let Some(s) = c.size(r, o, depth + 1, cache, allocate)? else {
                        return Ok(None);
                    };
                    n = n
                        .checked_add(s)
                        .ok_or_else(|| invalid("fixed size overflow"))?;
                }
                Some(n)
            }
            Self::Struct(i) => {
                let t = r.get(
                    *o.and_then(|t| t.depends.get(*i))
                        .ok_or_else(|| invalid("struct dependency"))?,
                )?;
                // Shared dependencies form a DAG, not necessarily a tree. The
                // depth is part of the key to preserve the recursion limit.
                let key = (t.code_hash, depth);
                if let Some(size) = cache.get(&key) {
                    return Ok(*size);
                }
                allocate(
                    (size_of::<(u64, usize)>() + size_of::<Option<usize>>()) * 12
                        + size_of::<usize>() * 16,
                )?;
                let mut n = 0usize;
                for f in &t.fields {
                    if f.is_extended {
                        cache.insert(key, None);
                        return Ok(None);
                    };
                    allocate(
                        f.code
                            .len()
                            .saturating_mul(size_of::<Code>())
                            .saturating_mul(3),
                    )?;
                    let Some(s) =
                        Self::from_words(&f.code)?.size(r, Some(t), depth + 1, cache, allocate)?
                    else {
                        cache.insert(key, None);
                        return Ok(None);
                    };
                    n = n
                        .checked_add(s)
                        .ok_or_else(|| invalid("fixed size overflow"))?;
                }
                cache.insert(key, Some(n));
                Some(n)
            }
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn alt(code: Code) -> Code {
        Code::Alt(Box::new(code))
    }
    #[test]
    fn mixed_endian_nodes_keep_independent_tags_and_parameters() {
        let code = alt(Code::Tuple(vec![
            Code::U16,
            alt(Code::Array(2, Box::new(Code::U32))),
            Code::Optional(Box::new(alt(Code::I64))),
        ]));
        let words = vec![
            0x1700, 0x0300, 0x0500, 0x0600, 0x0900, 2, 0x0b00, 0x0200, 3, 33, 0x0800,
        ];
        assert_eq!(code.to_words(), words);
        assert_eq!(Code::from_words(&words).unwrap(), code);
        let map = alt(Code::Map(Box::new(alt(Code::U16)), Box::new(Code::String)));
        assert_eq!(map.to_words(), [0x0d00, 0x0300, 0x0200, 32]);
        assert_eq!(Code::from_words(&map.to_words()).unwrap(), map);
        let map = Code::Map(Box::new(alt(Code::Struct(1))), Box::new(alt(Code::String)));
        assert_eq!(map.to_words(), [13, 4, 0x1300, 0x0100, 0x2000]);
        assert_eq!(Code::from_words(&map.to_words()).unwrap(), map);
    }
    #[test]
    fn alternate_matrices_images_and_scalars_roundtrip() {
        for code in [
            alt(Code::U64),
            alt(Code::Bool),
            alt(Code::TypeCode),
            alt(Code::Dynamic),
            alt(Code::List(Box::new(Code::I16))),
            alt(Code::Matrix(vec![3, 7], Box::new(Code::U16))),
            alt(Code::Image(2, Box::new(alt(Code::F32)))),
        ] {
            assert_eq!(Code::from_words(&code.to_words()).unwrap(), code);
        }
        let matrix = alt(Code::Matrix(vec![3, 7], Box::new(Code::U16)));
        assert_eq!(matrix.to_words(), [0x1500, 0x0200, 0x0300, 0x0700, 2]);
        assert_eq!(
            matrix.fixed_size(&crate::Registry::new(), None).unwrap(),
            Some(42)
        );
        assert_eq!(
            alt(Code::Tuple(vec![Code::U16, alt(Code::U64)]))
                .fixed_size(&crate::Registry::new(), None)
                .unwrap(),
            Some(10)
        );
    }
    #[test]
    fn malformed_alternate_headers_are_rejected() {
        for words in [
            vec![],
            vec![0x0b00],
            vec![0x1300],
            vec![0x1200],
            vec![0x0301],
            vec![0x0d00, 3, 2, 3],
            vec![0x1700, 0x0200, 0x0400, 0x0400, 3, 4],
            vec![0x1500, 0x2100, 3],
            vec![0x0c00, 0x0300, 0],
        ] {
            assert!(Code::from_words(&words).is_err(), "{words:x?}");
        }
    }
}
