use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    pub name: String,
    pub value: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeRef {
    pub name: String,
    pub args: Vec<TypeRef>,
    pub is_const: bool,
    pub is_static: bool,
    pub pointer: bool,
    pub array: Option<usize>,
}
impl TypeRef {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub ty: TypeRef,
    pub default: String,
    pub annotations: Vec<Annotation>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Method {
    pub name: String,
    pub params: Vec<Field>,
    pub returns: TypeRef,
    pub is_const: bool,
    pub is_async: bool,
    pub is_static: bool,
    pub annotations: Vec<Annotation>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Struct,
    Class,
    Enum,
    Module,
    Interface,
    Extern,
    Topic,
    Typedef,
    Method,
    Return,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Definition {
    pub name: String,
    pub package: String,
    pub kind: Kind,
    pub is_abstract: bool,
    pub parent: Option<TypeRef>,
    pub implements: Vec<TypeRef>,
    pub fields: Vec<Field>,
    pub constants: Vec<Field>,
    pub methods: Vec<Method>,
    pub aliases: BTreeMap<String, String>,
    pub enum_values: BTreeMap<String, u32>,
    pub code: Vec<u16>,
    pub annotations: Vec<Annotation>,
    pub imports: BTreeMap<String, String>,
    pub source: String,
    pub line: usize,
}
impl Definition {
    pub fn full_name(&self) -> String {
        format!("{}.{}", self.package, self.name)
    }
    pub fn is_class(&self) -> bool {
        matches!(self.kind, Kind::Class | Kind::Method | Kind::Return)
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub package: String,
    pub imports: BTreeMap<String, String>,
    pub definitions: Vec<Definition>,
}
