//! VNX v1/v2 schema-carrying streams. No native layout or C++ ABI is used.
use crate::{code::Code, error::invalid, Error, Field, Registry, Result, TypeCode};
use serde_json::{Map, Number, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read, Write},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ByteOrder {
    #[default]
    Little,
    Big,
}
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_depth: usize,
    pub max_container: usize,
    pub max_string_bytes: usize,
    pub max_value_bytes: usize,
    pub max_schemas: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 100,
            max_container: 1_000_000,
            max_string_bytes: 16 * 1024 * 1024,
            max_value_bytes: 64 * 1024 * 1024,
            max_schemas: 65536,
        }
    }
}

pub struct Reader<R> {
    input: R,
    registry: Registry,
    limits: Limits,
    consumed: usize,
    order: ByteOrder,
    pending: Option<u8>,
    data_order: ByteOrder,
    schema_orders: BTreeMap<u64, ByteOrder>,
    utf8_lossy: bool,
}
impl<R: Read> Reader<R> {
    pub fn new(input: R, registry: Registry) -> Self {
        Self {
            input,
            registry,
            limits: Limits::default(),
            consumed: 0,
            order: ByteOrder::Little,
            pending: None,
            data_order: ByteOrder::Little,
            schema_orders: BTreeMap::new(),
            utf8_lossy: false,
        }
    }
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
    pub fn with_byte_order(mut self, order: ByteOrder) -> Self {
        self.order = order;
        self.data_order = order;
        self
    }
    /// Decode native byte strings into a lossy JSON text view. Strict UTF-8 is
    /// the default. Consumers needing exact bytes must retain the input stream;
    /// this option is useful when inspecting payload boundaries or schemas.
    pub fn with_utf8_lossy(mut self, enabled: bool) -> Self {
        self.utf8_lossy = enabled;
        self
    }
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
    pub fn registry_mut(&mut self) -> &mut Registry {
        &mut self.registry
    }
    pub fn into_inner(self) -> R {
        self.input
    }
    pub fn read(&mut self) -> Result<Value> {
        self.consumed = 0;
        self.any(0)
    }
    /// Distinguishes clean EOF from a truncated value.
    pub fn read_next(&mut self) -> Result<Option<Value>> {
        let mut first = [0u8; 1];
        loop {
            match self.input.read(&mut first) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        self.pending = Some(first[0]);
        self.read().map(Some)
    }
    /// Database fields can carry their schema out of band.
    pub fn read_typed_payload(&mut self, hash: u64) -> Result<Value> {
        self.consumed = 0;
        let schema = self.registry.get(hash)?.clone();
        self.record(&schema, 0)
    }
    /// Read one entry from a schema-only stream.
    pub fn read_type_code(&mut self) -> Result<TypeCode> {
        self.consumed = 0;
        loop {
            let mut bytes = [0u8; 2];
            self.take_into(&mut bytes)?;
            match u16::from_le_bytes(bytes) {
                0xffff => continue,
                0x3713 => {
                    self.order = ByteOrder::Little;
                    continue;
                }
                0x1337 => {
                    self.order = ByteOrder::Big;
                    continue;
                }
                14 => self.order = ByteOrder::Little,
                0x0e00 => self.order = ByteOrder::Big,
                _ => return Err(invalid("expected TypeCode")),
            }
            return self.schema(0);
        }
    }
    pub fn read_payload(&mut self, code: &Code, owner: Option<&TypeCode>) -> Result<Value> {
        self.consumed = 0;
        self.value(code, owner, 0)
    }
    fn depth(&self, d: usize) -> Result<()> {
        if d >= self.limits.max_depth {
            Err(Error::Limit("recursion depth".into()))
        } else {
            Ok(())
        }
    }
    fn take_into(&mut self, b: &mut [u8]) -> Result<()> {
        self.consumed = self
            .consumed
            .checked_add(b.len())
            .filter(|n| *n <= self.limits.max_value_bytes)
            .ok_or_else(|| Error::Limit("value bytes".into()))?;
        let rest = if let Some(first) = self.pending.take() {
            if b.is_empty() {
                self.pending = Some(first);
                b
            } else {
                b[0] = first;
                &mut b[1..]
            }
        } else {
            b
        };
        self.input.read_exact(rest)?;
        Ok(())
    }
    fn raw(&mut self, n: usize) -> Result<Vec<u8>> {
        if n > self.limits.max_value_bytes.saturating_sub(self.consumed) {
            return Err(Error::Limit("value bytes".into()));
        }
        let mut v = vec![0; n];
        self.take_into(&mut v)?;
        Ok(v)
    }
    fn n(&mut self, n: usize) -> Result<u64> {
        let mut b = [0u8; 8];
        if self.order == ByteOrder::Big {
            self.take_into(&mut b[8 - n..])?;
            Ok(u64::from_be_bytes(b))
        } else {
            self.take_into(&mut b[..n])?;
            Ok(u64::from_le_bytes(b))
        }
    }
    fn count(&mut self) -> Result<usize> {
        let n = self.n(4)? as usize;
        if n > self.limits.max_container {
            Err(Error::Limit("container size".into()))
        } else {
            Ok(n)
        }
    }
    fn text(&mut self) -> Result<String> {
        let n = self.n(4)? as usize;
        if n > self.limits.max_string_bytes {
            return Err(Error::Limit("string bytes".into()));
        }
        let bytes = self.raw(n)?;
        self.decode_text(bytes)
    }
    fn decode_text(&self, bytes: Vec<u8>) -> Result<String> {
        if self.utf8_lossy {
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        } else {
            String::from_utf8(bytes).map_err(|_| invalid("invalid UTF-8"))
        }
    }
    fn words(&mut self, width: usize) -> Result<Vec<u16>> {
        let n = self.n(width)? as usize;
        if n == 0 || n > 32 {
            return Err(invalid("byte code length"));
        }
        let mut words = Vec::new();
        for _ in 0..n {
            words.push(self.n(2)? as u16);
        }
        Ok(words)
    }
    fn dynamic(&mut self, depth: usize) -> Result<Value> {
        let saved = self.order;
        // Dynamic headers independently describe their byte order.
        let mut size = [0u8; 2];
        self.take_into(&mut size)?;
        let raw = u16::from_le_bytes(size);
        self.order = if raw > 255 {
            ByteOrder::Big
        } else {
            ByteOrder::Little
        };
        let n = if raw > 255 { raw.swap_bytes() } else { raw } as usize;
        if n == 0 || n > 32 {
            return Err(invalid("dynamic byte code length"));
        }
        let mut words = Vec::with_capacity(n);
        for _ in 0..n {
            let mut b = [0u8; 2];
            self.take_into(&mut b)?;
            words.push(u16::from_le_bytes(b));
        }
        let code = Code::from_words(&words)?;
        let saved_data_order = self.data_order;
        self.data_order = ByteOrder::Little;
        let out = self.value(&code, None, depth + 1);
        self.data_order = saved_data_order;
        self.order = saved;
        out
    }
    fn any(&mut self, depth: usize) -> Result<Value> {
        self.depth(depth)?;
        loop {
            let mut b = [0u8; 2];
            self.take_into(&mut b)?;
            let raw = u16::from_le_bytes(b);
            if raw == 0xffff {
                continue;
            }
            if raw == 0x3713 {
                self.order = ByteOrder::Little;
                continue;
            }
            if raw == 0x1337 {
                self.order = ByteOrder::Big;
                continue;
            }
            let (c, order) = if raw > 255 {
                (raw.swap_bytes(), ByteOrder::Big)
            } else {
                (raw, ByteOrder::Little)
            };
            self.order = order;
            match c {
                0 => return Ok(Value::Null),
                16 => continue,
                18 => {
                    let n = self.n(4)? as usize;
                    self.raw(n)?;
                }
                14 => {
                    self.schema(depth + 1)?;
                }
                15 => {
                    let hash = self.n(8)?;
                    let schema = self.registry.get(hash)?.clone();
                    let mut value = self.record(&schema, depth + 1)?;
                    if let Some(object) = value.as_object_mut() {
                        object.insert("__type".into(), schema.name.into());
                    } else {
                        value = serde_json::json!({"__type":schema.name,"value":value});
                    }
                    return Ok(value);
                }
                17 => return self.dynamic(depth + 1),
                24 => {
                    let saved = self.data_order;
                    self.data_order = self.order;
                    let result = self.value(&Code::Object, None, depth + 1);
                    self.data_order = saved;
                    return result;
                }
                _ => return Err(invalid(format!("unexpected top-level code {c}"))),
            }
        }
    }
    fn field(&mut self, depth: usize) -> Result<Field> {
        let saved = self.order;
        let value = self.field_inner(depth);
        self.order = saved;
        value
    }
    fn field_inner(&mut self, depth: usize) -> Result<Field> {
        let mut b = [0u8; 2];
        self.take_into(&mut b)?;
        let raw = u16::from_le_bytes(b);
        self.order = if raw > 255 {
            ByteOrder::Big
        } else {
            ByteOrder::Little
        };
        let version = if raw > 255 { raw.swap_bytes() } else { raw } as u64;
        let n = match version {
            1 => 4,
            2 => self.n(2)?,
            _ => return Err(invalid("field version")),
        };
        if !(4..=64).contains(&n) {
            return Err(invalid("field header size"));
        }
        let ext = self.n(1)? != 0;
        let name = self.text()?;
        let mut code = self.words(4)?;
        let value = self.text()?;
        if version == 1 {
            for i in 0..code.len() {
                if code[i] == 15 && (i == 0 || ![11, 13, 15].contains(&code[i - 1])) {
                    code[i] = 19
                }
            }
        }
        Code::from_words(&code)?;
        let data_size = if n >= 5 {
            let v = self.dynamic(depth + 1)?;
            v.as_u64().ok_or_else(|| invalid("field size"))?
        } else {
            0
        };
        for _ in 5..n {
            self.dynamic(depth + 1)?;
        }
        Ok(Field {
            name,
            code,
            is_extended: ext,
            value,
            data_size,
        })
    }
    fn schema(&mut self, depth: usize) -> Result<TypeCode> {
        self.depth(depth)?;
        let mut version = self.n(2)?;
        while version == 14 {
            self.schema(depth + 1)?;
            version = self.n(2)?;
        }
        let n = match version {
            1 => 10,
            2 => self.n(2)?,
            _ => return Err(invalid("schema version")),
        };
        if !(10..=64).contains(&n) {
            return Err(invalid("schema header size"));
        }
        let type_hash = self.n(8)?;
        let code_hash = self.n(8)?;
        let return_type = self.n(8)?;
        let mut parents = Vec::new();
        for _ in 0..self.count()? {
            parents.push(self.n(8)?);
        }
        let mut depends = Vec::new();
        for _ in 0..self.count()? {
            depends.push(self.n(8)?);
        }
        let name = self.text()?;
        let mut fields = Vec::new();
        for _ in 0..self.count()? {
            fields.push(self.field(depth + 1)?);
        }
        let mut static_fields = Vec::new();
        for _ in 0..self.count()? {
            static_fields.push(self.field(depth + 1)?);
        }
        let mut methods = Vec::new();
        for _ in 0..self.count()? {
            methods.push(self.n(8)?);
        }
        let mut enum_map = BTreeMap::new();
        for _ in 0..self.count()? {
            let key = self.n(4)? as u32;
            let v = self.text()?;
            if enum_map.insert(key, v).is_some() {
                return Err(invalid("duplicate enum value"));
            }
        }
        let mut alias_map = BTreeMap::new();
        if n >= 11 {
            let v = self.dynamic(depth + 1)?;
            for (k, v) in map_entries(&v)? {
                alias_map.insert(
                    k.as_str().ok_or_else(|| invalid("alias key"))?.into(),
                    v.as_str().ok_or_else(|| invalid("alias value"))?.into(),
                );
            }
        }
        let mut flags = [false; 4];
        if n >= 12 {
            let v = self.dynamic(depth + 1)?;
            let v = array(&v)?;
            if v.len() != 4 {
                return Err(invalid("schema flags"));
            }
            for (i, v) in v.iter().enumerate() {
                flags[i] = v.as_bool().ok_or_else(|| invalid("schema flag"))?;
            }
        } else {
            flags[0] = !enum_map.is_empty();
            flags[1] = !parents.is_empty();
            flags[2] = return_type != 0;
        }
        let permission = if n >= 13 {
            self.dynamic(depth + 1)?
                .as_str()
                .ok_or_else(|| invalid("permission"))?
                .into()
        } else {
            String::new()
        };
        for _ in 13..n {
            self.dynamic(depth + 1)?;
        }
        let mut code = TypeCode {
            name,
            type_hash,
            code_hash,
            fields,
            static_fields,
            parents,
            depends,
            methods,
            return_type,
            enum_map,
            alias_map,
            is_enum: flags[0],
            is_class: flags[1],
            is_method: flags[2],
            is_return: flags[3],
            permission,
            ..Default::default()
        };
        for h in code
            .parents
            .iter()
            .chain(&code.depends)
            .chain(&code.methods)
            .chain((return_type != 0).then_some(return_type).iter())
        {
            self.registry.get(*h)?;
        }
        for fields in [&code.fields, &code.static_fields] {
            let mut names = BTreeSet::new();
            for f in fields {
                if !names.insert(&f.name) {
                    return Err(invalid("duplicate schema field"));
                }
                let c = Code::from_words(&f.code)?;
                validate_dependencies(&c, &code, self.limits.max_depth)?;
                if !f.is_extended {
                    let fixed = c
                        .fixed_size(&self.registry, Some(&code))?
                        .ok_or_else(|| invalid("variable inline field"))?;
                    if f.data_size > 0 && f.data_size < fixed as u64 {
                        return Err(invalid("inline field size"));
                    }
                    if f.data_size > self.limits.max_value_bytes as u64 {
                        return Err(Error::Limit("inline field size".into()));
                    }
                }
            }
        }
        for i in 0..code.fields.len() {
            if !code.fields[i].is_extended && code.fields[i].data_size == 0 {
                code.fields[i].data_size = Code::from_words(&code.fields[i].code)?
                    .fixed_size(&self.registry, Some(&code))?
                    .ok_or_else(|| invalid("inline size"))?
                    as u64;
            }
        }
        if self.registry.len() >= self.limits.max_schemas {
            return Err(Error::Limit("type codes".into()));
        }
        self.registry.register(code.clone())?;
        self.schema_orders.insert(code.code_hash, self.order);
        Ok(code)
    }
    fn record(&mut self, t: &TypeCode, depth: usize) -> Result<Value> {
        let saved = self.data_order;
        self.data_order = self
            .schema_orders
            .get(&t.code_hash)
            .copied()
            .unwrap_or(saved);
        let result = self.record_inner(t, depth);
        self.data_order = saved;
        result
    }
    fn record_inner(&mut self, t: &TypeCode, depth: usize) -> Result<Value> {
        self.depth(depth)?;
        let mut fields = Map::new();
        if t.is_class {
            fields.insert("__type".into(), t.name.clone().into());
        }
        for ext in [false, true] {
            for f in &t.fields {
                if f.is_extended == ext {
                    let before = self.consumed;
                    let code = Code::from_words(&f.code)?;
                    let value = self.value(&code, Some(t), depth + 1)?;
                    if !ext && f.data_size > 0 {
                        let used = self.consumed - before;
                        let size = usize::try_from(f.data_size)
                            .map_err(|_| invalid("field size overflow"))?;
                        if used > size {
                            return Err(invalid("inline field size"));
                        }
                        self.raw(size - used)?;
                    }
                    fields.insert(f.name.clone(), value);
                }
            }
        }
        if t.is_enum {
            let n = fields
                .get("value")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("enum value"))? as u32;
            return Ok(t
                .enum_map
                .get(&n)
                .map_or(Value::from(n), |s| Value::from(s.clone())));
        }
        Ok(Value::Object(fields))
    }
    fn value(&mut self, c: &Code, t: Option<&TypeCode>, depth: usize) -> Result<Value> {
        let saved = self.order;
        let (code, order) = match c {
            Code::Alt(code) => (code.as_ref(), ByteOrder::Big),
            _ => (c, self.data_order),
        };
        self.order = order;
        let result = self.value_inner(code, t, depth);
        self.order = saved;
        result
    }
    fn value_inner(&mut self, c: &Code, t: Option<&TypeCode>, depth: usize) -> Result<Value> {
        self.depth(depth)?;
        Ok(match c {
            Code::Alt(c) => return self.value(c, t, depth + 1),
            Code::Null => Value::Null,
            Code::Bool => Value::Bool(self.n(1)? != 0),
            Code::U8 => Value::from(self.n(1)?),
            Code::U16 => Value::from(self.n(2)?),
            Code::U32 => Value::from(self.n(4)?),
            Code::U64 => Value::from(self.n(8)?),
            Code::I8 => Value::from(self.n(1)? as i8),
            Code::I16 => Value::from(self.n(2)? as i16),
            Code::I32 => Value::from(self.n(4)? as i32),
            Code::I64 => Value::from(self.n(8)? as i64),
            Code::F32 => {
                let n = f32::from_bits(self.n(4)? as u32);
                Value::Number(
                    Number::from_f64(n as f64)
                        .ok_or_else(|| invalid("nonfinite floating point"))?,
                )
            }
            Code::F64 => Value::Number(
                Number::from_f64(f64::from_bits(self.n(8)?))
                    .ok_or_else(|| invalid("nonfinite floating point"))?,
            ),
            Code::String => self.text()?.into(),
            Code::Any => return self.any(depth + 1),
            Code::Dynamic => return self.dynamic(depth + 1),
            Code::Optional(c) => {
                if self.n(1)? != 0 {
                    self.value(c, t, depth + 1)?
                } else {
                    Value::Null
                }
            }
            Code::Array(n, c) => self.sequence(*n, c, t, depth + 1)?,
            Code::Matrix(d, c) => {
                let n = d.iter().try_fold(1usize, |n, d| {
                    n.checked_mul(*d).ok_or_else(|| invalid("matrix size"))
                })?;
                self.sequence(n, c, t, depth + 1)?
            }
            Code::List(c) => {
                let n = self.count()?;
                if matches!(c.as_ref(), Code::I8) {
                    let b = self.raw(n)?;
                    self.decode_text(b)
                        .map_err(|_| invalid("invalid UTF-8 character list"))?
                        .into()
                } else {
                    self.sequence(n, c, t, depth + 1)?
                }
            }
            Code::Map(k, v) => {
                let n = self.count()?;
                let mut list = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    list.push(Value::Array(vec![
                        self.value(k, t, depth + 1)?,
                        self.value(v, t, depth + 1)?,
                    ]));
                }
                Value::Array(list)
            }
            Code::Tuple(parts) => {
                let mut list = Vec::with_capacity(parts.len());
                for c in parts {
                    list.push(self.value(c, t, depth + 1)?)
                }
                Value::Array(list)
            }
            Code::Struct(i) => {
                let h = *t
                    .and_then(|t| t.depends.get(*i))
                    .ok_or_else(|| invalid("struct dependency"))?;
                let schema = self.registry.get(h)?.clone();
                self.record(&schema, depth + 1)?
            }
            Code::Object => {
                let mut fields = Map::new();
                for _ in 0..self.count()? {
                    let key = self.text()?;
                    let value = self.dynamic(depth + 1)?;
                    if fields.insert(key, value).is_some() {
                        return Err(invalid("duplicate object field"));
                    }
                }
                Value::Object(fields)
            }
            Code::TypeCode => serde_json::to_value(self.schema(depth + 1)?)?,
            Code::Image(d, c) => {
                let mut dims = Vec::with_capacity(*d);
                let mut n = 1usize;
                for _ in 0..*d {
                    let d = self.count()?;
                    n = n
                        .checked_mul(d)
                        .filter(|n| *n <= self.limits.max_container)
                        .ok_or_else(|| Error::Limit("image size".into()))?;
                    dims.push(d);
                }
                serde_json::json!({"size":dims,"data":self.sequence(n,c,t,depth+1)?})
            }
        })
    }
    fn sequence(
        &mut self,
        n: usize,
        c: &Code,
        t: Option<&TypeCode>,
        depth: usize,
    ) -> Result<Value> {
        if n > self.limits.max_container {
            return Err(Error::Limit("sequence size".into()));
        }
        let mut out = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            out.push(self.value(c, t, depth + 1)?)
        }
        Ok(Value::Array(out))
    }
}

pub struct Writer<W> {
    output: W,
    registry: Registry,
    emitted: BTreeSet<u64>,
    limits: Limits,
    order: ByteOrder,
    started: bool,
    written: usize,
    data_order: ByteOrder,
    type_codes: bool,
    header: bool,
}
impl<W: Write> Writer<W> {
    pub fn new(output: W, registry: Registry) -> Self {
        Self {
            output,
            registry,
            emitted: BTreeSet::new(),
            limits: Limits::default(),
            order: ByteOrder::Little,
            started: false,
            written: 0,
            data_order: ByteOrder::Little,
            type_codes: true,
            header: true,
        }
    }
    pub fn with_byte_order(mut self, order: ByteOrder) -> Self {
        self.order = order;
        self.data_order = order;
        self
    }
    /// Omit schemas when they are persisted separately from database values.
    pub fn with_type_codes(mut self, enabled: bool) -> Self {
        self.type_codes = enabled;
        self
    }
    pub fn with_header(mut self, enabled: bool) -> Self {
        self.header = enabled;
        self
    }
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
    pub fn registry_mut(&mut self) -> &mut Registry {
        &mut self.registry
    }
    pub fn into_inner(self) -> W {
        self.output
    }
    pub fn flush(&mut self) -> Result<()> {
        self.output.flush()?;
        Ok(())
    }
    pub fn write(&mut self, value: &Value) -> Result<()> {
        self.written = 0;
        if !self.started {
            if self.header {
                self.n(0xffff, 2)?;
                self.n(0x3713, 2)?;
            }
            self.started = true;
        }
        self.any(value, 0)
    }
    pub fn write_payload(
        &mut self,
        value: &Value,
        code: &Code,
        owner: Option<&TypeCode>,
    ) -> Result<()> {
        self.written = 0;
        self.value(value, code, owner, 0)
    }
    pub fn write_typed_payload(&mut self, value: &Value, hash: u64) -> Result<()> {
        self.written = 0;
        let schema = self.registry.get(hash)?.clone();
        self.record(value, &schema, 0)
    }
    /// Select an exact version even when several schemas have the same name.
    pub fn write_typed(&mut self, value: &Value, hash: u64) -> Result<()> {
        self.written = 0;
        if !self.started {
            if self.header {
                self.n(0xffff, 2)?;
                self.n(0x3713, 2)?;
            }
            self.started = true;
        }
        let schema = self.registry.get(hash)?.clone();
        if self.type_codes {
            self.schema(&schema, 0, &mut BTreeSet::new())?;
        }
        self.n(15, 2)?;
        self.n(schema.code_hash, 8)?;
        self.record(value, &schema, 0)
    }
    pub fn write_type_code(&mut self, code: &TypeCode) -> Result<()> {
        self.written = 0;
        self.schema(code, 0, &mut BTreeSet::new())
    }
    fn depth(&self, d: usize) -> Result<()> {
        if d >= self.limits.max_depth {
            Err(Error::Limit("recursion depth".into()))
        } else {
            Ok(())
        }
    }
    fn raw(&mut self, b: &[u8]) -> Result<()> {
        self.written = self
            .written
            .checked_add(b.len())
            .filter(|n| *n <= self.limits.max_value_bytes)
            .ok_or_else(|| Error::Limit("value bytes".into()))?;
        self.output.write_all(b)?;
        Ok(())
    }
    fn n(&mut self, n: u64, width: usize) -> Result<()> {
        let b = match self.order {
            ByteOrder::Little => n.to_le_bytes(),
            ByteOrder::Big => n.to_be_bytes(),
        };
        if self.order == ByteOrder::Big {
            self.raw(&b[8 - width..])
        } else {
            self.raw(&b[..width])
        }
    }
    fn count(&mut self, n: usize) -> Result<()> {
        if n > self.limits.max_container {
            return Err(Error::Limit("container size".into()));
        }
        self.n(
            u32::try_from(n).map_err(|_| invalid("length overflow"))? as u64,
            4,
        )
    }
    fn text(&mut self, s: &str) -> Result<()> {
        if s.len() > self.limits.max_string_bytes {
            return Err(Error::Limit("string bytes".into()));
        }
        self.n(
            u32::try_from(s.len()).map_err(|_| invalid("string length"))? as u64,
            4,
        )?;
        self.raw(s.as_bytes())
    }
    fn words(&mut self, words: &[u16], width: usize) -> Result<()> {
        if words.is_empty() || words.len() > 32 {
            return Err(invalid("byte code length"));
        }
        Code::from_words(words)?;
        self.n(words.len() as u64, width)?;
        for c in words {
            self.n(*c as u64, 2)?;
        }
        Ok(())
    }
    fn dynamic(&mut self, v: &Value, depth: usize) -> Result<()> {
        let saved = self.order;
        self.order = self.data_order;
        let result = self.dynamic_inner(v, depth);
        self.order = saved;
        result
    }
    fn dynamic_inner(&mut self, v: &Value, depth: usize) -> Result<()> {
        let code = match v {
            Value::Null => Code::Null,
            Value::Bool(_) => Code::Bool,
            Value::String(_) => Code::String,
            Value::Number(n) => {
                if let Some(n) = n.as_u64() {
                    match n {
                        0..=255 => Code::U8,
                        256..=65535 => Code::U16,
                        65536..=4294967295 => Code::U32,
                        _ => Code::U64,
                    }
                } else if let Some(n) = n.as_i64() {
                    match n {
                        -128..=-1 => Code::I8,
                        -32768..=-129 => Code::I16,
                        -2147483648..=-32769 => Code::I32,
                        _ => Code::I64,
                    }
                } else if n.to_string().contains(['.', 'e', 'E']) {
                    Code::F64
                } else {
                    return Err(invalid("integer exceeds 64 bits"));
                }
            }
            Value::Array(_) => Code::List(Box::new(Code::Dynamic)),
            Value::Object(o) => {
                if o.get("__type").and_then(Value::as_str).is_some() {
                    Code::Any
                } else {
                    Code::Object
                }
            }
        };
        self.words(&code.to_words(), 2)?;
        self.value(v, &code, None, depth + 1)
    }
    fn any(&mut self, v: &Value, depth: usize) -> Result<()> {
        let saved = self.order;
        self.order = self.data_order;
        let result = self.any_inner(v, depth);
        self.order = saved;
        result
    }
    fn any_inner(&mut self, v: &Value, depth: usize) -> Result<()> {
        self.depth(depth)?;
        if v.is_null() {
            return self.n(0, 2);
        }
        if let Some(name) = v.get("__type").and_then(Value::as_str) {
            let t = self.registry.by_name(name)?.clone();
            if self.type_codes {
                self.schema(&t, depth + 1, &mut BTreeSet::new())?;
            }
            self.n(15, 2)?;
            self.n(t.code_hash, 8)?;
            self.record(v, &t, depth + 1)
        } else if v.is_object() {
            self.n(24, 2)?;
            self.value(v, &Code::Object, None, depth + 1)
        } else {
            self.n(17, 2)?;
            self.dynamic(v, depth + 1)
        }
    }
    fn field(&mut self, f: &Field, depth: usize) -> Result<()> {
        self.n(2, 2)?;
        self.n(5, 2)?;
        self.n(u64::from(f.is_extended), 1)?;
        self.text(&f.name)?;
        self.words(&f.code, 4)?;
        self.text(&f.value)?;
        self.dynamic(&f.data_size.into(), depth + 1)
    }
    fn schema(&mut self, t: &TypeCode, depth: usize, active: &mut BTreeSet<u64>) -> Result<()> {
        self.schema_payload(t, depth, active, true)
    }
    fn schema_payload(
        &mut self,
        t: &TypeCode,
        depth: usize,
        active: &mut BTreeSet<u64>,
        tagged: bool,
    ) -> Result<()> {
        // Original codegen hashes native-endian integers. The registry contains
        // little-endian ABI hashes; merely flipping payload bytes would reuse
        // the wrong schema identity in existing C++ registries.
        if self.order == ByteOrder::Big {
            return Err(invalid("typed VNX streams require little-endian schema hashes; use big endian only for raw or dynamic payloads"));
        }
        if tagged && self.emitted.contains(&t.code_hash) {
            return Ok(());
        }
        self.depth(depth)?;
        if !active.insert(t.code_hash) {
            return Err(invalid("cyclic type code dependency"));
        }
        for h in t
            .parents
            .iter()
            .chain(&t.depends)
            .chain(&t.methods)
            .chain((t.return_type != 0).then_some(t.return_type).iter())
        {
            let dep = self.registry.get(*h)?.clone();
            self.schema(&dep, depth + 1, active)?;
        }
        active.remove(&t.code_hash);
        if tagged {
            self.n(14, 2)?;
        }
        self.n(2, 2)?;
        self.n(13, 2)?;
        self.n(t.type_hash, 8)?;
        self.n(t.code_hash, 8)?;
        self.n(t.return_type, 8)?;
        for list in [&t.parents, &t.depends] {
            self.count(list.len())?;
            for h in list {
                self.n(*h, 8)?;
            }
        }
        self.text(&t.name)?;
        for fields in [&t.fields, &t.static_fields] {
            self.count(fields.len())?;
            for f in fields {
                self.field(f, depth + 1)?;
            }
        }
        self.count(t.methods.len())?;
        for h in &t.methods {
            self.n(*h, 8)?;
        }
        self.count(t.enum_map.len())?;
        for (k, v) in &t.enum_map {
            self.n(*k as u64, 4)?;
            self.text(v)?;
        }
        let aliases = Value::Array(
            t.alias_map
                .iter()
                .map(|(k, v)| serde_json::json!([k, v]))
                .collect(),
        );
        self.words(&[13, 3, 32, 32], 2)?;
        self.value(
            &aliases,
            &Code::Map(Box::new(Code::String), Box::new(Code::String)),
            None,
            depth + 1,
        )?;
        self.words(&[11, 4, 31], 2)?;
        for flag in [t.is_enum, t.is_class, t.is_method, t.is_return] {
            self.n(u64::from(flag), 1)?;
        }
        self.dynamic(&Value::String(t.permission.clone()), depth + 1)?;
        self.emitted.insert(t.code_hash);
        Ok(())
    }
    fn record(&mut self, v: &Value, t: &TypeCode, depth: usize) -> Result<()> {
        self.depth(depth)?;
        if t.is_enum {
            let v = v.get("value").unwrap_or(v);
            let n = if let Some(s) = v.as_str() {
                t.enum_map
                    .iter()
                    .find(|(_, name)| name.as_str() == s)
                    .map(|(n, _)| *n as u64)
                    .ok_or_else(|| invalid(format!("unknown enum value {s}")))?
            } else {
                v.as_u64()
                    .or_else(|| v.get("value").and_then(Value::as_u64))
                    .ok_or_else(|| invalid("enum value"))?
            };
            return self.value(&n.into(), &Code::U32, Some(t), depth + 1);
        }
        let object = v
            .as_object()
            .ok_or_else(|| invalid(format!("{} requires an object", t.name)))?;
        for ext in [false, true] {
            for f in &t.fields {
                if f.is_extended == ext {
                    let mut val = object.get(&f.name);
                    if val.is_none() {
                        for (a, b) in &t.alias_map {
                            if a == &f.name {
                                val = object.get(b)
                            } else if b == &f.name {
                                val = object.get(a)
                            }
                            if val.is_some() {
                                break;
                            }
                        }
                    }
                    let c = Code::from_words(&f.code)?;
                    let fallback = if val.is_none() {
                        Some(default_value(&c, &f.value, &self.registry, Some(t), 0)?)
                    } else {
                        None
                    };
                    let val = val
                        .or(fallback.as_ref())
                        .ok_or_else(|| invalid("missing field"))?;
                    let before = self.written;
                    self.value(val, &c, Some(t), depth + 1)?;
                    if !ext && f.data_size > 0 {
                        let used = self.written - before;
                        let size =
                            usize::try_from(f.data_size).map_err(|_| invalid("field size"))?;
                        if used != size {
                            return Err(invalid(format!("inline field size for {}", f.name)));
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn value(&mut self, v: &Value, c: &Code, t: Option<&TypeCode>, depth: usize) -> Result<()> {
        let saved = self.order;
        let (code, order) = match c {
            Code::Alt(code) => (code.as_ref(), ByteOrder::Big),
            _ => (c, self.data_order),
        };
        self.order = order;
        let result = self.value_inner(v, code, t, depth);
        self.order = saved;
        result
    }
    fn value_inner(
        &mut self,
        v: &Value,
        c: &Code,
        t: Option<&TypeCode>,
        depth: usize,
    ) -> Result<()> {
        self.depth(depth)?;
        match c {
            Code::Alt(c) => return self.value(v, c, t, depth + 1),
            Code::Null => {}
            Code::Bool => self.n(
                u64::from(v.as_bool().ok_or_else(|| invalid("expected bool"))?),
                1,
            )?,
            Code::U8 | Code::U16 | Code::U32 | Code::U64 => {
                let n = v
                    .as_u64()
                    .ok_or_else(|| invalid("expected unsigned integer"))?;
                let width = match c {
                    Code::U8 => 1,
                    Code::U16 => 2,
                    Code::U32 => 4,
                    _ => 8,
                };
                if width < 8 && n >= (1u64 << (width * 8)) {
                    return Err(invalid("integer overflow"));
                }
                self.n(n, width)?;
            }
            Code::I8 | Code::I16 | Code::I32 | Code::I64 => {
                let n = v
                    .as_i64()
                    .ok_or_else(|| invalid("expected signed integer"))?;
                let width = match c {
                    Code::I8 => 1,
                    Code::I16 => 2,
                    Code::I32 => 4,
                    _ => 8,
                };
                if width < 8 && (n < -(1i64 << (width * 8 - 1)) || n >= (1i64 << (width * 8 - 1))) {
                    return Err(invalid("integer overflow"));
                }
                self.n(n as u64, width)?;
            }
            Code::F32 => {
                let n = v.as_f64().ok_or_else(|| invalid("expected float"))? as f32;
                if !n.is_finite() {
                    return Err(invalid("float overflow"));
                }
                self.n(n.to_bits() as u64, 4)?;
            }
            Code::F64 => {
                let n = v.as_f64().ok_or_else(|| invalid("expected float"))?;
                if !n.is_finite() {
                    return Err(invalid("nonfinite float"));
                }
                self.n(n.to_bits(), 8)?;
            }
            Code::String => self.text(v.as_str().ok_or_else(|| invalid("expected string"))?)?,
            Code::Any => self.any(v, depth + 1)?,
            Code::Dynamic => self.dynamic(v, depth + 1)?,
            Code::Optional(c) => {
                self.n(u64::from(!v.is_null()), 1)?;
                if !v.is_null() {
                    self.value(v, c, t, depth + 1)?
                }
            }
            Code::Array(n, c) => {
                let a = array(v)?;
                if a.len() != *n {
                    return Err(invalid("fixed array length"));
                }
                self.sequence(a, c, t, depth + 1)?;
            }
            Code::List(c) => {
                if let (Code::I8, Some(s)) = (c.as_ref(), v.as_str()) {
                    self.count(s.len())?;
                    self.raw(s.as_bytes())?;
                } else {
                    let a = array(v)?;
                    self.count(a.len())?;
                    self.sequence(a, c, t, depth + 1)?;
                }
            }
            Code::Map(k, c) => {
                let entries = map_entries(v)?;
                self.count(entries.len())?;
                for (key, value) in entries {
                    self.value(&key, k, t, depth + 1)?;
                    self.value(&value, c, t, depth + 1)?;
                }
            }
            Code::Tuple(parts) => {
                let a = array(v)?;
                if a.len() != parts.len() {
                    return Err(invalid("tuple length"));
                }
                for (v, c) in a.iter().zip(parts) {
                    self.value(v, c, t, depth + 1)?;
                }
            }
            Code::Struct(i) => {
                let h = *t
                    .and_then(|t| t.depends.get(*i))
                    .ok_or_else(|| invalid("struct dependency"))?;
                let t = self.registry.get(h)?.clone();
                self.record(v, &t, depth + 1)?;
            }
            Code::Object => {
                let o = v.as_object().ok_or_else(|| invalid("expected object"))?;
                self.count(o.len())?;
                for (k, v) in o {
                    self.text(k)?;
                    self.dynamic(v, depth + 1)?;
                }
            }
            Code::TypeCode => {
                let tc: TypeCode = serde_json::from_value(v.clone())?;
                self.schema_payload(&tc, depth + 1, &mut BTreeSet::new(), false)?;
            }
            Code::Matrix(d, c) => {
                let n = d.iter().try_fold(1usize, |n, d| {
                    n.checked_mul(*d).ok_or_else(|| invalid("matrix size"))
                })?;
                let a = array(v)?;
                if a.len() != n {
                    return Err(invalid("matrix length"));
                }
                self.sequence(a, c, t, depth + 1)?;
            }
            Code::Image(dim, c) => {
                let sizes = array(&v["size"])?;
                if sizes.len() != *dim {
                    return Err(invalid("image dimensions"));
                }
                let mut n = 1usize;
                for d in sizes {
                    let d = d
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(|| invalid("image dimension"))?;
                    self.count(d)?;
                    n = n
                        .checked_mul(d)
                        .filter(|n| *n <= self.limits.max_container)
                        .ok_or_else(|| Error::Limit("image size".into()))?;
                }
                let a = array(&v["data"])?;
                if a.len() != n {
                    return Err(invalid("image data length"));
                }
                self.sequence(a, c, t, depth + 1)?;
            }
        }
        Ok(())
    }
    fn sequence(
        &mut self,
        a: &[Value],
        c: &Code,
        t: Option<&TypeCode>,
        depth: usize,
    ) -> Result<()> {
        if a.len() > self.limits.max_container {
            return Err(Error::Limit("sequence size".into()));
        }
        for v in a {
            self.value(v, c, t, depth + 1)?;
        }
        Ok(())
    }
}

fn array(v: &Value) -> Result<&Vec<Value>> {
    v.as_array().ok_or_else(|| invalid("expected array"))
}
fn map_entries(v: &Value) -> Result<Vec<(Value, Value)>> {
    if let Some(object) = v.as_object() {
        return Ok(object
            .iter()
            .map(|(k, v)| (Value::String(k.clone()), v.clone()))
            .collect());
    }
    let mut entries = Vec::new();
    for entry in array(v)? {
        let pair = array(entry)?;
        if pair.len() != 2 {
            return Err(invalid("map pair length"));
        }
        entries.push((pair[0].clone(), pair[1].clone()));
    }
    Ok(entries)
}
fn validate_dependencies(c: &Code, t: &TypeCode, depth: usize) -> Result<()> {
    if depth == 0 {
        return Err(invalid("code depth"));
    }
    match c {
        Code::Struct(i) if *i >= t.depends.len() => return Err(invalid("struct dependency index")),
        Code::Alt(c)
        | Code::Array(_, c)
        | Code::List(c)
        | Code::Optional(c)
        | Code::Matrix(_, c)
        | Code::Image(_, c) => validate_dependencies(c, t, depth - 1)?,
        Code::Map(k, v) => {
            validate_dependencies(k, t, depth - 1)?;
            validate_dependencies(v, t, depth - 1)?;
        }
        Code::Tuple(parts) => {
            for c in parts {
                validate_dependencies(c, t, depth - 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn default_value(
    c: &Code,
    text: &str,
    r: &Registry,
    t: Option<&TypeCode>,
    depth: usize,
) -> Result<Value> {
    if depth >= 100 {
        return Err(invalid("recursive default"));
    }
    if !text.is_empty() {
        if let Ok(v) = serde_json::from_str(text) {
            return Ok(v);
        }
        if text.starts_with('"') {
            return Err(invalid("invalid default string"));
        }
        return Ok(text.into());
    }
    Ok(match c {
        Code::Alt(c) => return default_value(c, text, r, t, depth + 1),
        Code::Null | Code::Optional(_) | Code::Any | Code::Dynamic => Value::Null,
        Code::Bool => false.into(),
        Code::String => String::new().into(),
        Code::Object => Value::Object(Map::new()),
        Code::List(c) if matches!(c.as_ref(), Code::I8) => String::new().into(),
        Code::List(_) | Code::Map(_, _) => Value::Array(Vec::new()),
        Code::Array(n, c) => Value::Array(
            (0..*n)
                .map(|_| default_value(c, "", r, t, depth + 1))
                .collect::<Result<_>>()?,
        ),
        Code::Matrix(d, c) => {
            let n = d.iter().try_fold(1usize, |n, d| {
                n.checked_mul(*d).ok_or_else(|| invalid("matrix size"))
            })?;
            if n > 1_000_000 {
                return Err(Error::Limit("default size".into()));
            }
            Value::Array(
                (0..n)
                    .map(|_| default_value(c, "", r, t, depth + 1))
                    .collect::<Result<_>>()?,
            )
        }
        Code::Tuple(parts) => Value::Array(
            parts
                .iter()
                .map(|c| default_value(c, "", r, t, depth + 1))
                .collect::<Result<_>>()?,
        ),
        Code::Struct(i) => {
            let t = r.get(
                *t.and_then(|t| t.depends.get(*i))
                    .ok_or_else(|| invalid("struct dependency"))?,
            )?;
            if t.is_enum {
                0.into()
            } else {
                let mut o = Map::new();
                for f in &t.fields {
                    o.insert(
                        f.name.clone(),
                        default_value(
                            &Code::from_words(&f.code)?,
                            &f.value,
                            r,
                            Some(t),
                            depth + 1,
                        )?,
                    );
                }
                Value::Object(o)
            }
        }
        Code::Image(d, _) => serde_json::json!({"size":vec![0usize;*d],"data":[]}),
        Code::TypeCode => return Err(invalid("missing TypeCode field")),
        _ => 0.into(),
    })
}
/// Read one self-describing value and require complete input consumption.
pub fn decode(bytes: &[u8]) -> Result<(Value, Registry)> {
    let mut reader = Reader::new(Cursor::new(bytes), Registry::new());
    let value = reader.read()?;
    if reader.input.position() != bytes.len() as u64 {
        return Err(invalid("trailing bytes"));
    }
    Ok((value, reader.registry))
}
pub fn encode(value: &Value, registry: &Registry) -> Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::new(), registry.clone());
    writer.write(value)?;
    Ok(writer.into_inner())
}
