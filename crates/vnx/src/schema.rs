use crate::{error::invalid, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub code: Vec<u16>,
    pub is_extended: bool,
    pub value: String,
    pub data_size: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeCode {
    pub name: String,
    pub type_hash: u64,
    pub code_hash: u64,
    pub fields: Vec<Field>,
    pub static_fields: Vec<Field>,
    pub parents: Vec<u64>,
    pub depends: Vec<u64>,
    pub methods: Vec<u64>,
    pub return_type: u64,
    pub enum_map: BTreeMap<u32, String>,
    pub alias_map: BTreeMap<String, String>,
    pub is_enum: bool,
    pub is_class: bool,
    pub is_method: bool,
    pub is_return: bool,
    pub permission: String,
    #[serde(default)]
    pub is_const: bool,
    #[serde(default)]
    pub is_async: bool,
}
#[derive(Clone, Debug, Default)]
pub struct Registry {
    codes: BTreeMap<u64, TypeCode>,
    names: BTreeMap<String, u64>,
}
impl Registry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, code: TypeCode) -> Result<()> {
        if code.name.is_empty() {
            return Err(invalid("empty type name"));
        }
        if let Some(old) = self.codes.get(&code.code_hash) {
            let mut old_wire = old.clone();
            let mut new_wire = code.clone();
            old_wire.is_const = false;
            old_wire.is_async = false;
            new_wire.is_const = false;
            new_wire.is_async = false;
            if old_wire != new_wire {
                return Err(invalid("conflicting type code hash"));
            }
            return Ok(());
        }
        self.names.insert(code.name.clone(), code.code_hash);
        self.codes.insert(code.code_hash, code);
        Ok(())
    }
    pub fn get(&self, hash: u64) -> Result<&TypeCode> {
        self.codes
            .get(&hash)
            .or_else(|| self.codes.values().find(|t| t.type_hash == hash))
            .ok_or(crate::Error::UnknownType(hash))
    }
    pub fn by_name(&self, name: &str) -> Result<&TypeCode> {
        self.names
            .get(name)
            .and_then(|h| self.codes.get(h))
            .ok_or_else(|| invalid(format!("unknown type {name}")))
    }
    pub fn iter(&self) -> impl Iterator<Item = &TypeCode> {
        self.codes.values()
    }
    pub fn len(&self) -> usize {
        self.codes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }
    pub(crate) fn contains_code_hash(&self, hash: u64) -> bool {
        self.codes.contains_key(&hash)
    }
    pub fn from_json(s: &str) -> Result<Self> {
        let mut out = Self::new();
        for code in serde_json::from_str::<Vec<TypeCode>>(s)? {
            out.register(code)?;
        }
        Ok(out)
    }
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(
            &self.iter().collect::<Vec<_>>(),
        )?)
    }
}
