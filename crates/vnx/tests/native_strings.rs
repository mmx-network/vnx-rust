use std::io::Cursor;
use vnx::{binary::Reader, code::Code, Registry};
#[test]
fn native_byte_strings_have_an_explicit_lossy_view_while_default_is_strict() {
    for code in [Code::String, Code::List(Box::new(Code::I8))] {
        let bytes = [3, 0, 0, 0, b'a', 255, 0];
        assert!(Reader::new(Cursor::new(bytes), Registry::new())
            .read_payload(&code, None)
            .is_err());
        let mut cursor = Cursor::new(bytes);
        let value = Reader::new(&mut cursor, Registry::new())
            .with_utf8_lossy(true)
            .read_payload(&code, None)
            .unwrap();
        assert_eq!(value.as_str(), Some("a\u{fffd}\0"));
        assert_eq!(cursor.position(), bytes.len() as u64);
        assert_eq!(cursor.into_inner(), bytes);
    }
}
