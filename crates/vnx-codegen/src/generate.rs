use crate::{ast::*, Compiled};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use vnx::{error::invalid, Result};

fn ident(name: &str) -> String {
    let s = name.replace('.', "_");
    if [
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "static", "struct", "super", "trait", "true", "type",
        "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
        "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
    ]
    .contains(&s.as_str())
    {
        format!("r#{s}")
    } else if s == "self" || s == "Self" {
        format!("{s}_")
    } else {
        s
    }
}

fn type_path(d: &Definition) -> String {
    format!(
        "crate::{}::{}",
        d.package
            .split('.')
            .map(ident)
            .collect::<Vec<_>>()
            .join("::"),
        ident(&d.name)
    )
}
fn rust_type(t: &TypeRef, defs: &BTreeMap<String, Definition>) -> Result<String> {
    if let Some(n) = t.array {
        let mut inner = t.clone();
        inner.array = None;
        return Ok(format!("::vnx::Fixed<{}, {n}>", rust_type(&inner, defs)?));
    }
    if t.pointer {
        return Ok("::vnx::serde_json::Value".into());
    }
    let basic = match t.name.as_str() {
        "void" => "()",
        "bool" => "bool",
        "uchar" | "uint8" => "u8",
        "ushort" | "uint16" => "u16",
        "uint" | "uint32" => "u32",
        "ulong" | "uint64" => "u64",
        "char" | "int8" => "i8",
        "short" | "int16" => "i16",
        "int" | "int32" => "i32",
        "long" | "int64" => "i64",
        "float" | "float32" => "f32",
        "double" | "float64" => "f64",
        "string" => "String",
        _ => "",
    };
    if !basic.is_empty() {
        return Ok(basic.into());
    }
    if t.name.starts_with("@enum:") {
        return Ok("u32".into());
    }
    let args = t
        .args
        .iter()
        .map(|a| rust_type(a, defs))
        .collect::<Result<Vec<_>>>()?;
    Ok(match t.name.as_str() {
        "vector" | "list" => format!("Vec<{}>", args[0]),
        "set" => {
            if !orderable(&t.args[0], defs, &mut BTreeSet::new())? {
                return Err(invalid(format!(
                    "set element {} needs a Rust ordering implementation",
                    t.args[0].name
                )));
            }
            format!("::std::collections::BTreeSet<{}>", args[0])
        }
        "optional" => format!("Option<{}>", args[0]),
        "map" => {
            if !orderable(&t.args[0], defs, &mut BTreeSet::new())? {
                return Err(invalid(format!(
                    "map key {} needs a Rust ordering implementation",
                    t.args[0].name
                )));
            }
            format!("::vnx::Map<{}, {}>", args[0], args[1])
        }
        "tuple" | "pair" => format!("({},)", args.join(", ")),
        _ => type_path(
            defs.get(&t.name)
                .ok_or_else(|| invalid(format!("unknown generated type {}", t.name)))?,
        ),
    })
}
fn orderable(
    t: &TypeRef,
    defs: &BTreeMap<String, Definition>,
    active: &mut BTreeSet<String>,
) -> Result<bool> {
    if t.pointer {
        return Ok(false);
    }
    if t.name.starts_with("@enum:") {
        return Ok(true);
    }
    if ["float", "float32", "double", "float64"].contains(&t.name.as_str()) {
        return Ok(false);
    }
    if [
        "void", "bool", "uchar", "uint8", "ushort", "uint16", "uint", "uint32", "ulong", "uint64",
        "char", "int8", "short", "int16", "int", "int32", "long", "int64", "string",
    ]
    .contains(&t.name.as_str())
    {
        return Ok(true);
    }
    if !t.args.is_empty() {
        for a in &t.args {
            if !orderable(a, defs, active)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    if !active.insert(t.name.clone()) {
        return Ok(false);
    }
    let d = defs
        .get(&t.name)
        .ok_or_else(|| invalid(format!("unknown ordering type {}", t.name)))?;
    let result = if d.kind == Kind::Extern {
        code_orderable(&::vnx::code::Code::from_words(&d.code)?)
    } else if d.kind == Kind::Enum {
        true
    } else {
        let mut result = true;
        for f in all_fields(d, defs) {
            if !orderable(&f.ty, defs, active)? {
                result = false;
                break;
            }
        }
        result
    };
    active.remove(&t.name);
    Ok(result)
}
fn code_orderable(code: &::vnx::code::Code) -> bool {
    use ::vnx::code::Code;
    match code {
        Code::F32
        | Code::F64
        | Code::Any
        | Code::Dynamic
        | Code::Object
        | Code::TypeCode
        | Code::Struct(_) => false,
        Code::Array(_, c)
        | Code::List(c)
        | Code::Optional(c)
        | Code::Matrix(_, c)
        | Code::Image(_, c) => code_orderable(c),
        Code::Map(k, v) => code_orderable(k) && code_orderable(v),
        Code::Tuple(parts) => parts.iter().all(code_orderable),
        _ => true,
    }
}
fn extern_type(code: &[u16]) -> Result<String> {
    let Some(first) = code.first() else {
        return Err(invalid("empty extern code"));
    };
    Ok(match first {
        1 => "u8".into(),
        2 => "u16".into(),
        3 => "u32".into(),
        4 => "u64".into(),
        5 => "i8".into(),
        6 => "i16".into(),
        7 => "i32".into(),
        8 => "i64".into(),
        9 => "f32".into(),
        10 => "f64".into(),
        11 => {
            let n = code
                .get(1)
                .ok_or_else(|| invalid("truncated extern array code"))?;
            format!("::vnx::Fixed<{}, {n}>", extern_type(&code[2..])?)
        }
        12 => {
            if code.get(1) == Some(&5) {
                "String".into()
            } else {
                format!("Vec<{}>", extern_type(&code[1..])?)
            }
        }
        14 | 16 | 17 | 24 => "::vnx::serde_json::Value".into(),
        31 => "bool".into(),
        32 => "String".into(),
        33 => format!("Option<{}>", extern_type(&code[1..])?),
        _ => return Err(invalid(format!("unsupported extern code {code:?}"))),
    })
}
fn default_expr(f: &Field, defs: &BTreeMap<String, Definition>) -> Result<String> {
    if f.default.is_empty() {
        return Ok("Default::default()".into());
    }
    if f.default == "null" {
        return Ok(if f.ty.pointer {
            "::vnx::serde_json::Value::Null"
        } else {
            "None"
        }
        .into());
    }
    if let Some(d) = defs.get(&f.ty.name) {
        if d.kind == Kind::Enum {
            return Ok(format!(
                "{}::{}",
                type_path(d),
                ident(f.default.rsplit('.').next().unwrap())
            ));
        }
    }
    if f.default.starts_with('"') {
        return Ok(format!("{}.to_owned()", f.default));
    }
    if f.default == "true" || f.default == "false" {
        return Ok(f.default.clone());
    }
    if let Some(n) = crate::parser::parse_int(&f.default) {
        return Ok(format!("({n}_i128) as {}", rust_type(&f.ty, defs)?));
    }
    if f.default.parse::<f64>().is_ok() {
        return Ok(format!("{} as {}", f.default, rust_type(&f.ty, defs)?));
    }
    Err(invalid(format!(
        "unsupported default {:?} for {}",
        f.default, f.name
    )))
}
#[derive(Default)]
struct Package {
    definitions: Vec<String>,
    children: BTreeMap<String, Package>,
}
/// Generate one self-contained Rust module tree. Include it at the generated crate's root.
pub fn generate(compiled: &Compiled) -> Result<String> {
    let mut tree = Package::default();
    for (name, d) in &compiled.definitions {
        let mut node = &mut tree;
        for p in d.package.split('.') {
            node = node.children.entry(p.to_owned()).or_default();
        }
        node.definitions.push(name.clone());
    }
    let mut out = String::from("// Generated by vnxrustcodegen. Do not edit by hand.\n");
    fn emit(node: &Package, c: &Compiled, out: &mut String) -> Result<()> {
        for name in &node.definitions {
            let d = &c.definitions[name];
            emit_definition(d, c, out)?;
        }
        for (name, child) in &node.children {
            writeln!(out,"#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals, unused_variables, clippy::too_many_arguments)]\npub mod {} {{",ident(name)).unwrap();
            emit(child, c, out)?;
            out.push_str("}\n");
        }
        Ok(())
    }
    emit(&tree, compiled, &mut out)?;
    let schema = compiled.registry.to_json()?;
    writeln!(out,"/// VNX schema descriptors for every generated type.\npub const VNX_SCHEMA_JSON: &str = {:?};\npub fn vnx_registry() -> ::vnx::Result<::vnx::Registry> {{ ::vnx::Registry::from_json(VNX_SCHEMA_JSON) }}",schema).unwrap();
    Ok(out)
}
fn emit_definition(d: &Definition, c: &Compiled, out: &mut String) -> Result<()> {
    let name = ident(&d.name);
    let fullname = d.full_name();
    match d.kind {
        Kind::Extern => {
            writeln!(out, "pub type {name} = {};", extern_type(&d.code)?).unwrap();
            return Ok(());
        }
        Kind::Topic => {
            writeln!(out, "pub const {name}: &str = {:?};", fullname).unwrap();
            return Ok(());
        }
        Kind::Typedef => {
            writeln!(
                out,
                "pub type {name} = {};",
                rust_type(d.parent.as_ref().unwrap(), &c.definitions)?
            )
            .unwrap();
            return Ok(());
        }
        _ => {}
    }
    let code = c.registry.by_name(&fullname)?;
    let fields = all_fields(d, &c.definitions);
    let derives = if d.kind == Kind::Enum {
        "Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash"
    } else if orderable(
        &TypeRef::named(&fullname),
        &c.definitions,
        &mut BTreeSet::new(),
    )? {
        "Clone, Debug, PartialEq, Eq, PartialOrd, Ord, ::vnx::serde::Serialize, ::vnx::serde::Deserialize"
    } else {
        "Clone, Debug, PartialEq, ::vnx::serde::Serialize, ::vnx::serde::Deserialize"
    };
    writeln!(out, "#[derive({derives})]").unwrap();
    if d.kind != Kind::Enum {
        out.push_str("#[serde(crate = \"::vnx::serde\", default)]\n");
    }
    writeln!(out, "pub struct {name} {{").unwrap();
    for f in &fields {
        for (old, new) in &code.alias_map {
            if new == &f.name {
                writeln!(out, "#[serde(alias = {:?})]", old).unwrap();
            } else if old == &f.name && !fields.iter().any(|field| &field.name == new) {
                writeln!(out, "#[serde(alias = {:?})]", new).unwrap();
            }
        }
        let field = ident(&f.name);
        if field != f.name && !field.starts_with("r#") {
            writeln!(out, "#[serde(rename = {:?})]", f.name).unwrap();
        }
        writeln!(out, "pub {field}: {},", rust_type(&f.ty, &c.definitions)?).unwrap();
    }
    out.push_str("}\n");
    writeln!(
        out,
        "impl Default for {name} {{ fn default() -> Self {{ Self {{"
    )
    .unwrap();
    for f in &fields {
        writeln!(
            out,
            "{}: {},",
            ident(&f.name),
            default_expr(f, &c.definitions)?
        )
        .unwrap();
    }
    out.push_str("} } }\n");
    if d.kind == Kind::Enum {
        writeln!(out, "impl ::vnx::serde::Serialize for {name} {{ fn serialize<S: ::vnx::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {{ match self.value {{").unwrap();
        for (label, number) in &d.enum_values {
            writeln!(out, "{number}u32 => serializer.serialize_str({:?}),", label).unwrap();
        }
        out.push_str("_ => serializer.serialize_u32(self.value), } } }\n");
        writeln!(out, "impl<'de> ::vnx::serde::Deserialize<'de> for {name} {{ fn deserialize<D: ::vnx::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {{ let value = <::vnx::serde_json::Value as ::vnx::serde::Deserialize>::deserialize(deserializer)?; let value = value.get(\"value\").cloned().unwrap_or(value); let number = if let Some(number) = value.as_u64() {{ u32::try_from(number).map_err(::vnx::serde::de::Error::custom)? }} else if let Some(text) = value.as_str() {{ match text {{").unwrap();
        for (label, number) in &d.enum_values {
            writeln!(out, "{:?} => {number}u32,", label).unwrap();
        }
        writeln!(out, "_ => text.parse::<u32>().map_err(::vnx::serde::de::Error::custom)?, }} }} else {{ return Err(::vnx::serde::de::Error::custom(\"expected enum name or uint32\")); }}; Ok(Self {{ value: number }}) }} }}").unwrap();
    }
    writeln!(out,"impl ::vnx::VnxType for {name} {{ const TYPE_NAME: &'static str = {:?}; const TYPE_HASH: u64 = {}u64; const CODE_HASH: u64 = {}u64; }}",fullname,code.type_hash,code.code_hash).unwrap();
    let constants = all_constants(d, &c.definitions);
    if !constants.is_empty() || d.kind == Kind::Enum {
        writeln!(out, "impl {name} {{").unwrap();
        for (k, v) in &d.enum_values {
            writeln!(
                out,
                "pub const {}: Self = Self {{ value: {v}u32 }};",
                ident(k)
            )
            .unwrap();
        }
        for f in constants.values() {
            if f.default.starts_with('"') {
                writeln!(
                    out,
                    "pub const {}: &'static str = {};",
                    ident(&f.name),
                    f.default
                )
                .unwrap();
            } else {
                writeln!(
                    out,
                    "pub const {}: {} = {};",
                    ident(&f.name),
                    rust_type(&f.ty, &c.definitions)?,
                    default_expr(f, &c.definitions)?
                )
                .unwrap();
            }
        }
        out.push_str("}\n");
    }
    if matches!(d.kind, Kind::Module | Kind::Interface) || !code.methods.is_empty() {
        emit_rpc(d, c, out)?;
    }
    Ok(())
}
fn all_fields(d: &Definition, defs: &BTreeMap<String, Definition>) -> Vec<Field> {
    let mut out = d
        .parent
        .as_ref()
        .map(|p| all_fields(&defs[&p.name], defs))
        .unwrap_or_default();
    out.extend(d.fields.clone());
    out
}
fn all_constants(d: &Definition, defs: &BTreeMap<String, Definition>) -> BTreeMap<String, Field> {
    let mut out = d
        .parent
        .as_ref()
        .map(|p| all_constants(&defs[&p.name], defs))
        .unwrap_or_default();
    out.extend(
        d.constants
            .iter()
            .cloned()
            .map(|field| (field.name.clone(), field)),
    );
    out
}
fn rpc_methods(d: &Definition, c: &Compiled) -> Result<Vec<(String, Method)>> {
    let code = c.registry.by_name(&d.full_name())?;
    let mut methods = BTreeMap::new();
    for hash in &code.methods {
        let req = c.registry.get(*hash)?;
        let (owner, name) = req.name.rsplit_once('.').unwrap();
        let m = c.definitions[owner]
            .methods
            .iter()
            .find(|m| m.name == name)
            .unwrap();
        methods.insert(name.to_owned(), (req.name.clone(), m.clone()));
    }
    // A method declared by the derived module overrides the inherited method of the same name.
    for m in &d.methods {
        let fullname = format!("{}.{}", d.full_name(), m.name);
        if c.definitions.contains_key(&fullname) {
            methods.insert(m.name.clone(), (fullname, m.clone()));
        }
    }
    Ok(methods.into_values().collect())
}
fn emit_rpc(d: &Definition, c: &Compiled, out: &mut String) -> Result<()> {
    let name = ident(&d.name);
    let client = format!("{name}Client");
    let service = format!("{name}Service");
    let dispatcher = format!("{name}Dispatcher");
    let methods = rpc_methods(d, c)?;
    writeln!(out,"#[derive(Clone)]\npub struct {client}<C: ::vnx::rpc::RpcClient = ::vnx::runtime::Client> {{ pub inner: C }}\nimpl<C: ::vnx::rpc::RpcClient> {client}<C> {{\npub fn new(inner: C) -> Self {{ Self {{ inner }} }}").unwrap();
    for (full, m) in &methods {
        let mname = ident(&m.name);
        let returns = rust_type(&m.returns, &c.definitions)?;
        let req = type_path(&c.definitions[full]);
        let ret = type_path(&c.definitions[&format!("{full}.return")]);
        let args = m
            .params
            .iter()
            .map(|f| {
                Ok(format!(
                    "{}: {}",
                    ident(&f.name),
                    rust_type(&f.ty, &c.definitions)?
                ))
            })
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        let sep = if args.is_empty() { "" } else { ", " };
        writeln!(out,"pub fn {mname}(&self{sep}{args}) -> ::vnx::Result<{returns}> {{\nlet request = {req} {{ {} }};\nlet value = self.inner.call_value({:?}, ::vnx::VnxType::to_value(&request)?)?;",m.params.iter().map(|f|ident(&f.name)).collect::<Vec<_>>().join(", "),full).unwrap();
        if m.returns.name == "void" {
            writeln!(
                out,
                "let _response = <{ret} as ::vnx::VnxType>::from_value(value)?; Ok(())\n}}"
            )
            .unwrap();
        } else {
            writeln!(out,"if value.get(\"_ret_0\").is_some() {{ Ok(<{ret} as ::vnx::VnxType>::from_value(value)?._ret_0) }} else {{ Ok(::vnx::serde_json::from_value(value)?) }}\n}}").unwrap();
        }
        if m.is_async {
            writeln!(out,"pub fn {mname}_async(&self{sep}{args}) -> ::vnx::Result<::vnx::runtime::RpcCall> {{ let request = {req} {{ {} }}; self.inner.call_async_value({:?}, ::vnx::VnxType::to_value(&request)?) }}",m.params.iter().map(|f|ident(&f.name)).collect::<Vec<_>>().join(", "),full).unwrap();
        }
    }
    out.push_str("}\n");
    writeln!(out,"/// Implement the module's RPC methods; RequestContext carries caller identity.\npub trait {service}: Send + Sync + 'static {{").unwrap();
    for (_, m) in &methods {
        let args = m
            .params
            .iter()
            .map(|f| {
                Ok(format!(
                    "{}: {}",
                    ident(&f.name),
                    rust_type(&f.ty, &c.definitions)?
                ))
            })
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        let sep = if args.is_empty() { "" } else { ", " };
        writeln!(out,"fn {}(&self{sep}{args}, context: &::vnx::runtime::RequestContext) -> ::vnx::Result<{}>;",ident(&m.name),rust_type(&m.returns,&c.definitions)?).unwrap();
    }
    out.push_str("}\n");
    writeln!(out,"pub struct {dispatcher}<T: {service}>(pub T);\nimpl<T: {service}> ::vnx::runtime::Service for {dispatcher}<T> {{\nfn call(&self, method: &str, args: ::vnx::serde_json::Value, context: &::vnx::runtime::RequestContext) -> ::vnx::Result<::vnx::serde_json::Value> {{\nmatch method {{").unwrap();
    for (full, m) in &methods {
        let req = type_path(&c.definitions[full]);
        let ret = type_path(&c.definitions[&format!("{full}.return")]);
        let args = m
            .params
            .iter()
            .map(|f| format!("request.{}", ident(&f.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let sep = if args.is_empty() { "" } else { ", " };
        writeln!(out,"{:?} => {{ let request = <{req} as ::vnx::VnxType>::from_value(args)?; let result = self.0.{}({args}{sep}context)?; ::vnx::VnxType::to_value(&{ret} {{ {} }}) }},",full,ident(&m.name),if m.returns.name=="void"{String::new()}else{"_ret_0: result".into()}).unwrap();
    }
    out.push_str("_ => Err(::vnx::error::invalid(format!(\"unknown method {method}\"))),\n} }\nfn permission(&self, method: &str) -> Option<String> { match method {\n");
    for (full, method) in &methods {
        let declared = &c.registry.by_name(full)?.permission;
        let permission = if declared.is_empty() {
            if method.is_const {
                "vnx.permission_e.CONST_REQUEST"
            } else {
                "vnx.permission_e.REQUEST"
            }
        } else {
            declared
        };
        writeln!(out, "{:?} => Some({:?}.to_owned()),", full, permission).unwrap();
    }
    out.push_str("_ => None,\n} } }\n");
    Ok(())
}
