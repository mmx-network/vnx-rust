use crate::ast::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use vnx::{
    error::invalid,
    hash::{hash64, Crc64},
    Registry, Result, TypeCode,
};

#[derive(Clone, Debug, Default)]
pub struct Compiler {
    pub definitions: BTreeMap<String, Definition>,
}
#[derive(Clone, Debug)]
pub struct Compiled {
    pub definitions: BTreeMap<String, Definition>,
    pub registry: Registry,
}
fn crc(parts: &[&[u8]]) -> u64 {
    let mut c = Crc64::new();
    for p in parts {
        c.update(p);
    }
    c.as_u64()
}
fn primitive(name: &str) -> Option<(u16, usize, bool)> {
    Some(match name {
        "bool" => (31, 1, false),
        "uchar" | "uint8" => (1, 1, false),
        "ushort" | "uint16" => (2, 2, false),
        "uint" | "uint32" => (3, 4, false),
        "ulong" | "uint64" => (4, 8, false),
        "char" | "int8" => (5, 1, true),
        "short" | "int16" => (6, 2, true),
        "int" | "int32" => (7, 4, true),
        "long" | "int64" => (8, 8, true),
        "float" | "float32" => (9, 4, false),
        "double" | "float64" => (10, 8, false),
        _ => return None,
    })
}
fn builtin(name: &str) -> bool {
    primitive(name).is_some()
        || [
            "void", "string", "vector", "list", "set", "map", "tuple", "pair", "optional",
        ]
        .contains(&name)
}
impl Compiler {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add_source(&mut self, name: &str, text: &str) -> Result<()> {
        self.add_document(crate::parser::parse(name, text)?)
    }
    pub fn add_document(&mut self, doc: Document) -> Result<()> {
        for d in doc.definitions {
            let name = d.full_name();
            if self.definitions.contains_key(&name) {
                return Err(invalid(format!("duplicate declaration {name}")));
            }
            self.definitions.insert(name, d);
        }
        Ok(())
    }
    /// Add all .vni files in a directory recursively, in deterministic order.
    pub fn add_path(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if path.is_dir() {
            let mut entries = std::fs::read_dir(path)?
                .map(|e| e.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            entries.sort();
            for p in entries {
                if p.is_dir() || p.extension().is_some_and(|s| s == "vni") {
                    self.add_path(p)?;
                }
            }
        } else {
            self.add_source(&path.display().to_string(), &std::fs::read_to_string(path)?)?;
        }
        Ok(())
    }
    pub fn compile(mut self) -> Result<Compiled> {
        self.definitions
            .entry("vnx.Value".into())
            .or_insert_with(|| Definition {
                name: "Value".into(),
                package: "vnx".into(),
                kind: Kind::Class,
                is_abstract: true,
                parent: None,
                implements: vec![],
                fields: vec![],
                constants: vec![],
                methods: vec![],
                aliases: BTreeMap::new(),
                enum_values: BTreeMap::new(),
                code: vec![],
                annotations: vec![],
                imports: BTreeMap::new(),
                source: "<builtin>".into(),
                line: 0,
            });
        let original = self.definitions.clone();
        for d in self.definitions.values_mut() {
            if d.kind == Kind::Extern {
                vnx::code::Code::from_words(&d.code)?;
            }
            if !d.implements.is_empty() && d.kind != Kind::Module {
                return Err(invalid(format!(
                    "{}: only modules may implement interfaces",
                    d.full_name()
                )));
            }
            if d.parent.is_none() && d.is_class() && d.full_name() != "vnx.Value" {
                d.parent = Some(TypeRef::named("vnx.Value"));
            }
            if d.parent.is_none() && d.kind == Kind::Module && !d.is_abstract {
                d.parent = Some(TypeRef::named("vnx.Module"));
            }
            if let Some(p) = &mut d.parent {
                if matches!(
                    d.kind,
                    Kind::Enum | Kind::Interface | Kind::Extern | Kind::Topic
                ) {
                    return Err(invalid(
                        "only structs, classes and modules may extend a type",
                    ));
                }
                resolve_ref(p, &d.package, &d.imports, &original)?;
                if d.kind != Kind::Typedef && !original.contains_key(&p.name) {
                    return Err(invalid(
                        "super type must be a declared struct, class or module",
                    ));
                }
                if d.kind == Kind::Module && original[&p.name].kind != Kind::Module {
                    return Err(invalid("module super type must be a module"));
                }
                if d.kind != Kind::Typedef
                    && original
                        .get(&p.name)
                        .is_some_and(|parent| parent.kind == Kind::Extern)
                {
                    return Err(invalid("super type cannot be extern"));
                }
            }
            for p in &mut d.implements {
                resolve_ref(p, &d.package, &d.imports, &original)?;
                if !original
                    .get(&p.name)
                    .is_some_and(|interface| interface.kind == Kind::Interface)
                {
                    return Err(invalid(format!(
                        "{}.{} implements non-interface {}",
                        d.package, d.name, p.name
                    )));
                }
            }
            for f in d.fields.iter_mut().chain(d.constants.iter_mut()) {
                resolve_ref(&mut f.ty, &d.package, &d.imports, &original)?;
            }
            for m in &mut d.methods {
                resolve_ref(&mut m.returns, &d.package, &d.imports, &original)?;
                for f in &mut m.params {
                    resolve_ref(&mut f.ty, &d.package, &d.imports, &original)?;
                }
            }
            for a in &mut d.annotations {
                resolve_annotation(a, &d.package, &d.imports, &original)?;
            }
            if d.kind == Kind::Enum {
                d.fields.push(Field {
                    name: "value".into(),
                    ty: TypeRef::named(format!("@enum:{}", d.name)),
                    default: String::new(),
                    annotations: vec![],
                });
            }
        }
        let parents = self.definitions.clone();
        let mut generated = Vec::new();
        for d in self.definitions.values() {
            let mut names = BTreeSet::new();
            for m in &d.methods {
                if m.name == "handle" {
                    if m.params.len() != 1 || m.is_async || m.is_const {
                        return Err(invalid(format!(
                            "{}: handle requires one class parameter and cannot be const/async",
                            d.full_name()
                        )));
                    }
                    continue;
                }
                if !names.insert(&m.name) {
                    return Err(invalid(format!(
                        "duplicate method {}.{}",
                        d.full_name(),
                        m.name
                    )));
                }
                if m.is_static {
                    if m.is_async {
                        return Err(invalid("static methods cannot be async"));
                    }
                    continue;
                }
                if !matches!(d.kind, Kind::Module | Kind::Interface)
                    && !m.annotations.iter().any(|a| a.name == "Interface")
                {
                    continue;
                }
                if d.kind == Kind::Module && !m.is_const && m.returns.name != "void" {
                    return Err(invalid(format!(
                        "{}.{}: non-const module method must return void",
                        d.full_name(),
                        m.name
                    )));
                }
                let mut annotations = m.annotations.clone();
                for a in &mut annotations {
                    resolve_annotation(a, &d.package, &d.imports, &parents)?;
                }
                let request = Definition {
                    name: format!("{}.{}", d.name, m.name),
                    package: d.package.clone(),
                    kind: Kind::Method,
                    is_abstract: false,
                    parent: Some(TypeRef::named("vnx.Value")),
                    implements: vec![],
                    fields: m.params.clone(),
                    constants: vec![],
                    methods: vec![],
                    aliases: BTreeMap::new(),
                    enum_values: BTreeMap::new(),
                    code: vec![],
                    annotations,
                    imports: d.imports.clone(),
                    source: d.source.clone(),
                    line: d.line,
                };
                let mut returns = request.clone();
                returns.name.push_str(".return");
                returns.kind = Kind::Return;
                returns.annotations.clear();
                returns.fields = if m.returns.name == "void" {
                    vec![]
                } else {
                    vec![Field {
                        name: "_ret_0".into(),
                        ty: m.returns.clone(),
                        default: String::new(),
                        annotations: vec![],
                    }]
                };
                generated.extend([request, returns]);
            }
        }
        for d in generated {
            if self.definitions.insert(d.full_name(), d).is_some() {
                return Err(invalid("generated method type conflicts with declaration"));
            }
        }
        let mut hashes = BTreeMap::new();
        let mut registry = Registry::new();
        for (name, d) in &self.definitions {
            if matches!(d.kind, Kind::Extern | Kind::Topic | Kind::Typedef) {
                continue;
            }
            let code_hash =
                definition_hash(name, &self.definitions, &mut hashes, &mut BTreeSet::new())?;
            let mut aliases = BTreeMap::new();
            let mut fields = Vec::new();
            gather(
                name,
                &self.definitions,
                &mut fields,
                &mut aliases,
                &mut BTreeSet::new(),
            )?;
            let mut depends = Vec::new();
            let mut outfields = Vec::new();
            let mut names = BTreeSet::new();
            for f in fields {
                if !names.insert(f.name.clone()) {
                    return Err(invalid(format!(
                        "{name}: duplicate inherited field {}",
                        f.name
                    )));
                }
                let mut code = Vec::new();
                make_code(&f.ty, &self.definitions, &mut depends, &mut code)?;
                let size = value_size(&f.ty);
                let extended = size == 0 || f.annotations.iter().any(|a| a.name == "External");
                let value = default_text(&f, &self.definitions)?;
                outfields.push(vnx::Field {
                    name: f.name,
                    code,
                    is_extended: extended,
                    value,
                    data_size: if extended { 0 } else { size as u64 },
                });
            }
            for (old, new) in &aliases {
                if !names.contains(old) && !names.contains(new) {
                    return Err(invalid(format!(
                        "{name}: alias does not refer to a field: {old} {new}"
                    )));
                }
            }
            let mut parents = Vec::new();
            let mut parent = d.parent.as_ref();
            let mut visited = BTreeSet::new();
            while let Some(p) = parent {
                if !visited.insert(p.name.clone()) {
                    return Err(invalid("inheritance cycle"));
                }
                let pd = &self.definitions[&p.name];
                if !pd.is_abstract {
                    parents.push(definition_hash(
                        &p.name,
                        &self.definitions,
                        &mut hashes,
                        &mut BTreeSet::new(),
                    )?);
                }
                parent = pd.parent.as_ref();
            }
            let mut public = BTreeSet::new();
            gather_methods(name, &self.definitions, &mut public, &mut BTreeSet::new())?;
            let methods = public
                .iter()
                .map(|n| definition_hash(n, &self.definitions, &mut hashes, &mut BTreeSet::new()))
                .collect::<Result<Vec<_>>>()?;
            let return_type = if d.kind == Kind::Method {
                definition_hash(
                    &format!("{name}.return"),
                    &self.definitions,
                    &mut hashes,
                    &mut BTreeSet::new(),
                )?
            } else {
                0
            };
            let (mut is_const, mut is_async) = (false, false);
            if d.kind == Kind::Method {
                if let Some((owner, method)) = name.rsplit_once('.') {
                    if let Some(m) = self
                        .definitions
                        .get(owner)
                        .and_then(|o| o.methods.iter().find(|m| m.name == method))
                    {
                        is_const = m.is_const;
                        is_async = m.is_async;
                    }
                }
            }
            registry.register(TypeCode {
                name: name.clone(),
                type_hash: hash64(name.as_bytes()),
                code_hash,
                fields: outfields,
                static_fields: vec![],
                parents,
                depends: depends
                    .iter()
                    .map(|n| {
                        definition_hash(n, &self.definitions, &mut hashes, &mut BTreeSet::new())
                    })
                    .collect::<Result<Vec<_>>>()?,
                methods,
                return_type,
                enum_map: d.enum_values.iter().map(|(k, v)| (*v, k.clone())).collect(),
                alias_map: aliases,
                is_enum: d.kind == Kind::Enum,
                is_class: d.is_class(),
                is_method: d.kind == Kind::Method,
                is_return: d.kind == Kind::Return,
                is_const,
                is_async,
                permission: d
                    .annotations
                    .iter()
                    .find(|a| a.name == "Permission")
                    .map(|a| a.value.clone())
                    .unwrap_or_default(),
            })?;
        }
        Ok(Compiled {
            definitions: self.definitions,
            registry,
        })
    }
}
fn resolve_ref(
    t: &mut TypeRef,
    package: &str,
    imports: &BTreeMap<String, String>,
    defs: &BTreeMap<String, Definition>,
) -> Result<()> {
    resolve_ref_at_depth(t, package, imports, defs, 0)
}
fn resolve_ref_at_depth(
    t: &mut TypeRef,
    package: &str,
    imports: &BTreeMap<String, String>,
    defs: &BTreeMap<String, Definition>,
    depth: usize,
) -> Result<()> {
    if depth > 128 {
        return Err(invalid("type alias/template resolution exceeds 128 levels"));
    }
    if !builtin(&t.name) {
        let name = resolve_name(&t.name, package, imports, defs)?;
        let d = &defs[&name];
        if d.kind == Kind::Typedef {
            let mut target = d
                .parent
                .clone()
                .ok_or_else(|| invalid("typedef missing target"))?;
            resolve_ref_at_depth(&mut target, &d.package, &d.imports, defs, depth + 1)?;
            t.name = target.name;
            t.args = target.args;
        } else {
            t.name = name;
        }
    }
    for arg in &mut t.args {
        resolve_ref_at_depth(arg, package, imports, defs, depth + 1)?;
    }
    let arity = match t.name.as_str() {
        "vector" | "list" | "set" | "optional" => Some(1),
        "map" | "pair" => Some(2),
        _ => None,
    };
    if let Some(n) = arity {
        if t.args.len() != n {
            return Err(invalid(format!("{} requires {n} type parameters", t.name)));
        }
    } else if t.name == "tuple" {
        if t.args.is_empty() {
            return Err(invalid("tuple requires type parameters"));
        }
    } else if !t.args.is_empty() {
        return Err(invalid(format!("{} does not take type parameters", t.name)));
    }
    if t.pointer && !defs.get(&t.name).is_some_and(|d| d.is_class()) {
        return Err(invalid(format!("pointer requires class type: {}", t.name)));
    }
    if t.is_static {
        t.is_const = true;
    }
    Ok(())
}
fn resolve_name(
    name: &str,
    package: &str,
    imports: &BTreeMap<String, String>,
    defs: &BTreeMap<String, Definition>,
) -> Result<String> {
    if let Some(n) = imports.get(name) {
        if defs.contains_key(n) {
            return Ok(n.clone());
        }
    }
    let local = format!("{package}.{name}");
    if defs.contains_key(&local) {
        Ok(local)
    } else if defs.contains_key(name) {
        Ok(name.into())
    } else {
        Err(invalid(format!(
            "unresolved type {name} in package {package}"
        )))
    }
}
fn resolve_annotation(
    a: &mut Annotation,
    package: &str,
    imports: &BTreeMap<String, String>,
    defs: &BTreeMap<String, Definition>,
) -> Result<()> {
    if a.name != "Permission" {
        return Ok(());
    }
    let mut possibilities = vec![
        a.value.clone(),
        format!("{package}.{}", a.value),
        format!("vnx.permission_e.{}", a.value),
    ];
    if let Some((first, tail)) = a.value.split_once('.') {
        if let Some(full) = imports.get(first) {
            possibilities.push(format!("{full}.{tail}"));
        }
    }
    for p in possibilities {
        if let Some((en, value)) = p.rsplit_once('.') {
            if defs
                .get(en)
                .is_some_and(|d| d.enum_values.contains_key(value))
            {
                a.value = p;
                return Ok(());
            }
        }
    }
    Err(invalid(format!("unresolved permission {}", a.value)))
}
fn value_size(t: &TypeRef) -> usize {
    if t.pointer {
        0
    } else if let Some(n) = t.array {
        let mut inner = t.clone();
        inner.array = None;
        n.saturating_mul(value_size(&inner))
    } else if t.name.starts_with("@enum:") {
        4
    } else {
        primitive(&t.name).map(|x| x.1).unwrap_or(0)
    }
}
fn gather(
    name: &str,
    defs: &BTreeMap<String, Definition>,
    out: &mut Vec<Field>,
    aliases: &mut BTreeMap<String, String>,
    visited: &mut BTreeSet<String>,
) -> Result<()> {
    if !visited.insert(name.into()) {
        return Err(invalid("inheritance cycle"));
    }
    if visited.len() > 128 {
        return Err(invalid("inheritance exceeds 128 levels"));
    }
    let d = &defs[name];
    if let Some(p) = &d.parent {
        gather(&p.name, defs, out, aliases, visited)?;
    }
    out.extend(d.fields.clone());
    aliases.extend(d.aliases.clone());
    visited.remove(name);
    Ok(())
}
fn gather_methods(
    name: &str,
    defs: &BTreeMap<String, Definition>,
    out: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) -> Result<()> {
    if !visited.insert(name.into()) {
        return Err(invalid("method inheritance cycle"));
    }
    let d = &defs[name];
    if let Some(p) = &d.parent {
        gather_methods(&p.name, defs, out, visited)?;
    }
    for p in &d.implements {
        gather_methods(&p.name, defs, out, visited)?;
    }
    for m in &d.methods {
        let n = format!("{name}.{}", m.name);
        if defs.get(&n).is_some_and(|d| d.kind == Kind::Method) {
            out.insert(n);
        }
    }
    visited.remove(name);
    Ok(())
}
fn definition_hash(
    name: &str,
    defs: &BTreeMap<String, Definition>,
    cache: &mut BTreeMap<String, u64>,
    active: &mut BTreeSet<String>,
) -> Result<u64> {
    if let Some(x) = cache.get(name) {
        return Ok(*x);
    }
    if !active.insert(name.into()) {
        return Err(invalid(format!("type value/inheritance cycle at {name}")));
    }
    if active.len() > 128 {
        return Err(invalid("type dependencies exceed 128 levels"));
    }
    let d = defs
        .get(name)
        .ok_or_else(|| invalid(format!("unknown type {name}")))?;
    let value = if d.kind == Kind::Extern {
        let mut c = Crc64::new();
        for x in &d.code {
            c.update(x.to_le_bytes());
        }
        c.as_u64()
    } else if d.kind == Kind::Topic {
        hash64(d.name.as_bytes())
    } else if d.kind == Kind::Typedef {
        definition_hash(
            &d.parent
                .as_ref()
                .ok_or_else(|| invalid("typedef missing target"))?
                .name,
            defs,
            cache,
            active,
        )?
    } else {
        let mut base = Crc64::new();
        if let Some(p) = &d.parent {
            base.update(typename_hash(p, defs, cache, active)?.to_le_bytes());
        }
        base.update(name.as_bytes());
        let base = base.as_u64();
        if d.kind == Kind::Interface {
            base
        } else {
            let mut s = Crc64::new();
            s.update(base.to_le_bytes());
            let mut fields = Vec::new();
            let mut aliases = BTreeMap::new();
            gather(name, defs, &mut fields, &mut aliases, &mut BTreeSet::new())?;
            for f in &fields {
                let h = typename_hash(&f.ty, defs, cache, active)?;
                s.update(
                    crc(&[&h.to_le_bytes(), f.name.as_bytes(), f.default.as_bytes()]).to_le_bytes(),
                );
            }
            for (k, v) in aliases {
                s.update(k.as_bytes());
                s.update(v.as_bytes());
            }
            let s = s.as_u64();
            if d.kind == Kind::Enum {
                let mut c = Crc64::new();
                c.update(s.to_le_bytes());
                for (k, v) in &d.enum_values {
                    c.update(k.as_bytes());
                    c.update(v.to_le_bytes());
                }
                c.as_u64()
            } else {
                s
            }
        }
    };
    active.remove(name);
    cache.insert(name.into(), value);
    Ok(value)
}
fn typename_hash(
    t: &TypeRef,
    defs: &BTreeMap<String, Definition>,
    cache: &mut BTreeMap<String, u64>,
    active: &mut BTreeSet<String>,
) -> Result<u64> {
    let base = if let Some(n) = t.array {
        let mut inner = t.clone();
        inner.array = None;
        inner.is_const = false;
        inner.is_static = false;
        crc(&[
            b"array",
            &typename_hash(&inner, defs, cache, active)?.to_le_bytes(),
            &(n as i32).to_le_bytes(),
        ])
    } else if t.pointer {
        hash64(b"pointer")
    } else if let Some(name) = t.name.strip_prefix("@enum:") {
        let name = format!("{name}.enum_t");
        let p = crc(&[name.as_bytes(), &4_i32.to_le_bytes()]);
        crc(&[&p.to_le_bytes(), &[0]])
    } else if let Some((code, size, signed)) = primitive(&t.name) {
        let p = crc(&[t.name.as_bytes(), &(size as i32).to_le_bytes()]);
        if code == 31 || code == 9 || code == 10 {
            p
        } else {
            crc(&[&p.to_le_bytes(), &[u8::from(signed)]])
        }
    } else if builtin(&t.name) {
        hash64(t.name.as_bytes())
    } else {
        definition_hash(&t.name, defs, cache, active)?
    };
    let mut c = Crc64::new();
    c.update(base.to_le_bytes());
    if t.array.is_none() && !t.pointer {
        for a in &t.args {
            c.update(typename_hash(a, defs, cache, active)?.to_le_bytes());
        }
    }
    let is_const = if t.pointer { false } else { t.is_const };
    c.update([u8::from(is_const)]);
    c.update([u8::from(t.is_static)]);
    Ok(c.as_u64())
}
fn make_code(
    t: &TypeRef,
    defs: &BTreeMap<String, Definition>,
    depends: &mut Vec<String>,
    out: &mut Vec<u16>,
) -> Result<()> {
    if let Some(n) = t.array {
        out.extend([11, n as u16]);
        let mut inner = t.clone();
        inner.array = None;
        return make_code(&inner, defs, depends, out);
    }
    if t.pointer {
        out.push(16);
    } else if t.name.starts_with("@enum:") {
        out.push(3);
    } else if let Some((code, _, _)) = primitive(&t.name) {
        out.push(code);
    } else {
        match t.name.as_str() {
            "void" => return Err(invalid("void is only valid as a method return type")),
            "string" => out.push(32),
            "vector" | "list" | "set" => {
                out.push(12);
                make_code(&t.args[0], defs, depends, out)?;
            }
            "optional" => {
                out.push(33);
                make_code(&t.args[0], defs, depends, out)?;
            }
            "map" => {
                let start = out.len();
                out.extend([13, 0]);
                make_code(&t.args[0], defs, depends, out)?;
                out[start + 1] = (out.len() - start)
                    .try_into()
                    .map_err(|_| invalid("type code too long"))?;
                make_code(&t.args[1], defs, depends, out)?;
            }
            "pair" | "tuple" => {
                let start = out.len();
                out.extend([23, t.args.len() as u16]);
                out.resize(out.len() + t.args.len(), 0);
                for (i, a) in t.args.iter().enumerate() {
                    out[start + 2 + i] = (out.len() - start)
                        .try_into()
                        .map_err(|_| invalid("type code too long"))?;
                    make_code(a, defs, depends, out)?;
                }
            }
            _ => {
                let d = &defs[&t.name];
                if d.kind == Kind::Extern {
                    out.extend(&d.code);
                } else {
                    let index = if let Some(i) = depends.iter().position(|n| n == &t.name) {
                        i
                    } else {
                        depends.push(t.name.clone());
                        depends.len() - 1
                    };
                    out.extend([
                        19,
                        index
                            .try_into()
                            .map_err(|_| invalid("too many dependencies"))?,
                    ]);
                }
            }
        }
    }
    Ok(())
}
fn default_text(f: &Field, defs: &BTreeMap<String, Definition>) -> Result<String> {
    if f.default.is_empty() {
        return Ok(String::new());
    }
    if f.default == "null" {
        return Ok("null".into());
    }
    if let Some(d) = defs.get(&f.ty.name) {
        if d.kind == Kind::Enum {
            let name = f.default.rsplit('.').next().unwrap();
            if d.enum_values.contains_key(name) {
                return Ok(format!("\"{name}\""));
            }
            return Err(invalid(format!("invalid enum default {}", f.default)));
        }
    }
    if let Some((_, size, false)) = primitive(&f.ty.name) {
        if crate::parser::parse_int(&f.default).is_some_and(|n| n < 0) {
            let n = crate::parser::parse_int(&f.default).unwrap();
            return Ok(((n as u128) & ((1_u128 << (size * 8)) - 1)).to_string());
        }
    }
    Ok(f.default.clone())
}
