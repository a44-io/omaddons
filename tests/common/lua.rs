//! The two SavedVariables writer formats (M3-D17), rendered by one renderer that the
//! BugGrabber builders ([`super::buggrabber`]) and the parser's parse-back property test
//! share. As the clients write them (structure measured on 79 files, and 18 3.3.5a
//! files byte by byte): a leading blank line, CRLF line endings, one `NAME = { … }` per
//! global, only bracketed keys, a trailing comma after every field, and double-quoted
//! strings escaping only `\\`, `\"` and newline, every other byte raw (control bytes
//! included). [`Writer::Modern`] indents nothing and numbers nothing; [`Writer::Legacy`]
//! (3.3.5a) indents with tabs and follows each positional field with `-- [n]`.

/// A client's SavedVariables writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writer {
    /// Every current client: no indentation, no `-- [n]`.
    Modern,
    /// 3.3.5a: tab indentation, `-- [n]` after each positional field.
    Legacy,
}

impl Writer {
    pub const ALL: [Self; 2] = [Self::Modern, Self::Legacy];
}

/// A value as a client stores it.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Num(f64),
    /// Raw bytes: UTF-8 in practice, control bytes and all.
    Str(Vec<u8>),
    Table(Table),
}

impl Value {
    pub fn str(text: &str) -> Self {
        Self::Str(text.as_bytes().to_vec())
    }
}

/// A table's fields in the order they are written.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table(pub Vec<Field>);

/// One field: positional (numbered `-- [n]` by the legacy writer) or under a key.
#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    Pos(Value),
    Key(Key, Value),
}

/// A bracketed key.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    Str(String),
    Int(i64),
}

impl Table {
    /// The table with `value` under the string key `key`.
    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.0.push(Field::Key(Key::Str(key.to_owned()), value));
        self
    }

    /// The table with `value` as its next positional field.
    pub fn push(mut self, value: Value) -> Self {
        self.0.push(Field::Pos(value));
        self
    }
}

const EOL: &[u8] = b"\r\n";

/// A SavedVariables file of `globals`, in `writer`'s format.
pub fn render(writer: Writer, globals: &[(&str, Value)]) -> Vec<u8> {
    let mut out = EOL.to_vec();
    for (name, value) in globals {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b" = ");
        value_into(&mut out, writer, value, 0);
        out.extend_from_slice(EOL);
    }
    out
}

fn value_into(out: &mut Vec<u8>, writer: Writer, value: &Value, depth: usize) {
    match value {
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Int(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Value::Num(x) => out.extend_from_slice(x.to_string().as_bytes()),
        Value::Str(bytes) => string_into(out, bytes),
        Value::Table(table) => {
            out.push(b'{');
            out.extend_from_slice(EOL);
            let mut position = 0;
            for field in &table.0 {
                indent(out, writer, depth + 1);
                let value = match field {
                    Field::Pos(value) => {
                        position += 1;
                        value
                    }
                    Field::Key(key, value) => {
                        out.push(b'[');
                        match key {
                            Key::Str(s) => string_into(out, s.as_bytes()),
                            Key::Int(n) => out.extend_from_slice(n.to_string().as_bytes()),
                        }
                        out.extend_from_slice(b"] = ");
                        value
                    }
                };
                value_into(out, writer, value, depth + 1);
                out.push(b',');
                if writer == Writer::Legacy && matches!(field, Field::Pos(_)) {
                    out.extend_from_slice(format!(" -- [{position}]").as_bytes());
                }
                out.extend_from_slice(EOL);
            }
            indent(out, writer, depth);
            out.push(b'}');
        }
    }
}

fn indent(out: &mut Vec<u8>, writer: Writer, depth: usize) {
    if writer == Writer::Legacy {
        out.extend(std::iter::repeat_n(b'\t', depth));
    }
}

/// `"…"`, escaping `\`, `"` and newline; every other byte raw.
fn string_into(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'"');
    for &b in bytes {
        match b {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\n' => out.extend_from_slice(b"\\n"),
            _ => out.push(b),
        }
    }
    out.push(b'"');
}
