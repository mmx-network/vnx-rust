use crate::ast::*;
use std::collections::BTreeMap;
use vnx::{error::invalid, Result};

#[derive(Clone, Debug)]
struct Token {
    text: String,
    line: usize,
}
fn lex(text: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = text.chars().collect();
    let (mut i, mut line, mut out) = (0, 1, Vec::new());
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            line += usize::from(c == '\n');
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    i += 2;
                    closed = true;
                    break;
                }
                line += usize::from(chars[i] == '\n');
                i += 1;
            }
            if !closed {
                return Err(invalid(format!("line {line}: unterminated comment")));
            }
            continue;
        }
        let start = i;
        let start_line = line;
        if c == '"' {
            i += 1;
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    i += 1;
                    closed = true;
                    break;
                }
                line += usize::from(chars[i] == '\n');
                i += 1;
            }
            if !closed {
                return Err(invalid(format!("line {line}: unterminated string")));
            }
        } else if ",:;()[]<>!@#$^&=?~%{}*/".contains(c) {
            i += 1;
        } else {
            while i < chars.len()
                && !chars[i].is_whitespace()
                && !",:;()[]<>!@#$^&=?~%{}*/\"".contains(chars[i])
            {
                i += 1;
            }
        }
        out.push(Token {
            text: chars[start..i].iter().collect(),
            line: start_line,
        });
    }
    Ok(out)
}
struct Parser {
    tokens: Vec<Token>,
    at: usize,
    source: String,
}
impl Parser {
    fn peek(&self) -> &str {
        self.tokens
            .get(self.at)
            .map(|t| t.text.as_str())
            .unwrap_or("")
    }
    fn take(&mut self) -> Result<String> {
        let t = self
            .tokens
            .get(self.at)
            .ok_or_else(|| self.err("unexpected end of file"))?
            .text
            .clone();
        self.at += 1;
        Ok(t)
    }
    fn consume(&mut self, s: &str) -> bool {
        if self.peek() == s {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn err(&self, message: &str) -> vnx::Error {
        invalid(format!(
            "{}:{}: {message} (found {:?})",
            self.source,
            self.tokens
                .get(self.at)
                .map(|t| t.line)
                .unwrap_or_else(|| self.tokens.last().map(|t| t.line).unwrap_or(1)),
            self.peek()
        ))
    }
    fn expect(&mut self, s: &str) -> Result<()> {
        if self.consume(s) {
            Ok(())
        } else {
            Err(self.err(&format!("expected {s:?}")))
        }
    }
    fn ident(&mut self) -> Result<String> {
        let value = self.take()?;
        if value.is_empty()
            || !value.chars().next().unwrap().is_alphabetic() && !value.starts_with('_')
            || !value
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            return Err(self.err("expected identifier"));
        }
        Ok(value)
    }
    fn annotations(&mut self) -> Result<Vec<Annotation>> {
        let mut out = Vec::new();
        while self.consume("@") {
            let name = self.ident()?;
            if !["Permission", "Interface", "External"].contains(&name.as_str()) {
                return Err(self.err("unknown annotation"));
            }
            let value = if self.consume("(") {
                let x = self.take()?;
                self.expect(")")?;
                x
            } else {
                String::new()
            };
            if name == "Permission" && value.is_empty() {
                return Err(self.err("@Permission requires an enum value"));
            }
            out.push(Annotation { name, value });
        }
        Ok(out)
    }
    fn ty(&mut self, depth: usize) -> Result<TypeRef> {
        if depth > 128 {
            return Err(self.err("type nesting exceeds 128"));
        }
        let is_static = self.consume("static");
        let is_const = self.consume("const") || is_static;
        let name = self.ident()?;
        let mut args = Vec::new();
        if self.consume("<") {
            loop {
                args.push(self.ty(depth + 1)?);
                if self.consume(">") {
                    break;
                }
                self.expect(",")?;
            }
        }
        let pointer = self.consume("*");
        let mut ty = TypeRef {
            name,
            args,
            is_const,
            is_static,
            pointer,
            array: None,
        };
        self.array(&mut ty)?;
        Ok(ty)
    }
    fn array(&mut self, ty: &mut TypeRef) -> Result<()> {
        if self.consume("[") {
            let value = self.take()?;
            let size: usize = value
                .parse()
                .map_err(|_| self.err("invalid array length"))?;
            if size == 0 || size > 65535 {
                return Err(self.err("array length must be 1..65535"));
            }
            if ty.array.is_some() {
                return Err(self.err("multidimensional arrays are not supported by VNI"));
            }
            ty.array = Some(size);
            self.expect("]")?;
        }
        Ok(())
    }
    fn document(&mut self) -> Result<Document> {
        self.expect("package")?;
        let package = self.ident()?;
        self.expect(";")?;
        let mut imports = BTreeMap::new();
        while self.consume("import") {
            let name = self.ident()?;
            imports.insert(name.rsplit('.').next().unwrap().to_owned(), name);
            self.expect(";")?;
        }
        let mut definitions = Vec::new();
        while !self.peek().is_empty() {
            if self.consume(";") {
                continue;
            }
            let annotations = self.annotations()?;
            let is_abstract = self.consume("abstract");
            let keyword = self.take()?;
            let kind = match keyword.as_str() {
                "struct" => Kind::Struct,
                "class" => Kind::Class,
                "enum" => Kind::Enum,
                "module" => Kind::Module,
                "interface" => Kind::Interface,
                "extern" => Kind::Extern,
                "topic" => Kind::Topic,
                "typedef" => Kind::Typedef,
                _ => return Err(self.err("expected type declaration")),
            };
            let line = self.tokens.get(self.at).map(|t| t.line).unwrap_or(1);
            let mut name = self.ident()?;
            let mut parent = None;
            if kind == Kind::Typedef {
                parent = Some(TypeRef::named(name));
                name = self.ident()?;
            }
            let mut d = Definition {
                name,
                package: package.clone(),
                kind,
                is_abstract,
                parent,
                implements: Vec::new(),
                fields: Vec::new(),
                constants: Vec::new(),
                methods: Vec::new(),
                aliases: BTreeMap::new(),
                enum_values: BTreeMap::new(),
                code: Vec::new(),
                annotations,
                imports: imports.clone(),
                source: self.source.clone(),
                line,
            };
            if self.consume("extends") {
                d.parent = Some(self.ty(0)?);
            }
            if self.consume("implements") {
                loop {
                    d.implements.push(self.ty(0)?);
                    if !self.consume(",") {
                        break;
                    }
                }
            }
            if self.consume(";") {
                d.is_abstract = true;
                definitions.push(d);
                continue;
            }
            if d.kind == Kind::Extern {
                self.expect("=")?;
                self.expect("{")?;
                while !self.consume("}") {
                    let token = self.take()?;
                    let n = parse_int(&token).ok_or_else(|| self.err("invalid extern code"))?;
                    if !(0..=65535).contains(&n) {
                        return Err(self.err("extern code exceeds uint16"));
                    }
                    d.code.push(n as u16);
                    if !self.consume(",") {
                        self.expect("}")?;
                        break;
                    }
                }
                if d.code.is_empty() {
                    return Err(self.err("extern code must not be empty"));
                }
                self.expect(";")?;
                definitions.push(d);
                continue;
            }
            self.expect("{")?;
            if d.kind == Kind::Enum {
                while !self.consume("}") {
                    let name = self.ident()?;
                    let value = if self.consume("=") {
                        parse_int(&self.take()?).ok_or_else(|| self.err("invalid enum value"))?
                            as u32
                    } else {
                        vnx::hash::hash64(name.as_bytes()) as u32
                    };
                    if d.enum_values.insert(name, value).is_some() {
                        return Err(self.err("duplicate enum name"));
                    }
                    if !self.consume(",") {
                        self.expect("}")?;
                        break;
                    }
                }
            } else {
                while !self.consume("}") {
                    let annotations = self.annotations()?;
                    if self.consume("alias") {
                        let from = self.ident()?;
                        let to = self.ident()?;
                        self.expect(";")?;
                        d.aliases.insert(from, to);
                        continue;
                    }
                    let mut ty = self.ty(0)?;
                    let name = self.ident()?;
                    if self.consume("(") {
                        let mut params = Vec::new();
                        while !self.consume(")") {
                            let mut ty = self.ty(0)?;
                            let name = self.ident()?;
                            self.array(&mut ty)?;
                            let default = if self.consume("=") {
                                self.take()?
                            } else {
                                String::new()
                            };
                            params.push(Field {
                                name,
                                ty,
                                default,
                                annotations: Vec::new(),
                            });
                            if !self.consume(",") {
                                self.expect(")")?;
                                break;
                            }
                        }
                        let is_static = ty.is_static;
                        ty.is_static = false;
                        let (mut is_const, mut is_async) = (false, false);
                        while self.peek() != ";" {
                            if self.consume("const") {
                                is_const = true;
                            } else if self.consume("async") {
                                is_async = true;
                            } else {
                                return Err(self.err("expected const, async or semicolon"));
                            }
                        }
                        self.expect(";")?;
                        d.methods.push(Method {
                            name,
                            params,
                            returns: ty,
                            is_static,
                            is_const,
                            is_async,
                            annotations,
                        });
                    } else {
                        if !matches!(d.kind, Kind::Struct | Kind::Class | Kind::Module) {
                            return Err(self.err("fields require a struct, class or module"));
                        }
                        self.array(&mut ty)?;
                        let default = if self.consume("=") {
                            self.take()?
                        } else {
                            String::new()
                        };
                        self.expect(";")?;
                        let field = Field {
                            name,
                            ty,
                            default,
                            annotations,
                        };
                        if field.ty.is_static {
                            if field.default.is_empty() {
                                return Err(self.err("static field needs a value"));
                            }
                            d.constants.push(field);
                        } else {
                            d.fields.push(field);
                        }
                    }
                }
            }
            self.consume(";");
            definitions.push(d);
        }
        Ok(Document {
            package,
            imports,
            definitions,
        })
    }
}
pub(crate) fn parse_int(s: &str) -> Option<i128> {
    let (neg, s) = if let Some(x) = s.strip_prefix('-') {
        (true, x)
    } else {
        (false, s)
    };
    let n = if let Some(x) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i128::from_str_radix(x, 16).ok()?
    } else {
        s.parse().ok()?
    };
    Some(if neg { -n } else { n })
}
/// Parse one VNI document. Name resolution happens when compiling the complete set.
pub fn parse(source: &str, text: &str) -> Result<Document> {
    Parser {
        tokens: lex(text)?,
        at: 0,
        source: source.to_owned(),
    }
    .document()
}
