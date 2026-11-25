//! skkserv wire protocol.
//!
//! | request      | meaning    | response                          |
//! |--------------|------------|-----------------------------------|
//! | `0`          | disconnect | none                              |
//! | `1<key> `    | lookup     | `1/a/b/\n`, or `4<key> \n` on miss |
//! | `2`          | version    | `<name>.<version> `               |
//! | `3`          | host       | `<host>:<addr>: `                 |
//! | `4<prefix> ` | completion | same as lookup                    |

use encoding_rs::Encoding;
use std::fmt::Write as _;
use std::io::{self, BufRead, Write};

const MAX_KEY_BYTES: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Disconnect,
    Lookup(Vec<u8>),
    Version,
    Host,
    Complete(Vec<u8>),
}

/// Source of candidates for the protocol loop.
pub trait Lookup {
    fn lookup(&self, reading: &str) -> Vec<String>;
    fn complete(&self, prefix: &str) -> Vec<String>;
}

fn read_byte<R: BufRead>(reader: &mut R) -> io::Result<Option<u8>> {
    let byte = match reader.fill_buf()? {
        [] => return Ok(None),
        [byte, ..] => *byte,
    };
    reader.consume(1);
    Ok(Some(byte))
}

fn read_key<R: BufRead>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut key = Vec::new();
    while let Some(byte) = read_byte(reader)? {
        if matches!(byte, b' ' | b'\n') {
            return Ok(Some(key));
        }
        if key.len() == MAX_KEY_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "key too long"));
        }
        key.push(byte);
    }
    Ok(None)
}

/// Read one request. `None` means the peer closed the connection.
pub fn read_request<R: BufRead>(reader: &mut R) -> io::Result<Option<Request>> {
    let command = loop {
        match read_byte(reader)? {
            None => return Ok(None),
            Some(b' ' | b'\r' | b'\n' | b'\t') => {}
            Some(byte) => break byte,
        }
    };
    let keyed = |key: Option<Vec<u8>>, make: fn(Vec<u8>) -> Request| key.map(make);
    Ok(match command {
        b'0' => Some(Request::Disconnect),
        b'1' => keyed(read_key(reader)?, Request::Lookup),
        b'2' => Some(Request::Version),
        b'3' => Some(Request::Host),
        b'4' => keyed(read_key(reader)?, Request::Complete),
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown command byte 0x{other:02x}"),
            ));
        }
    })
}

/// Make a candidate safe to place between `/` delimiters.
///
/// SKK clients treat `;` as an annotation separator, and ddskk *evaluates*
/// candidates starting with `(` as Lisp. Model output is untrusted, so those
/// are wrapped in a `concat` of a literal string with the metacharacters in
/// octal. Candidates with control characters are rejected.
pub fn escape_candidate(candidate: &str) -> Option<String> {
    if candidate.is_empty() || candidate.chars().any(char::is_control) {
        return None;
    }
    if !candidate.starts_with('(') && !candidate.contains(['/', ';']) {
        return Some(candidate.to_string());
    }
    let mut escaped = String::from("(concat \"");
    for c in candidate.chars() {
        match c {
            '/' | ';' | '"' | '\\' | '(' | ')' => {
                // Writing to a String cannot fail.
                let _ = write!(escaped, "\\{:03o}", u32::from(c));
            }
            _ => escaped.push(c),
        }
    }
    escaped.push_str("\")");
    Some(escaped)
}

/// Encode a candidate line, dropping candidates the encoding cannot represent.
pub fn encode_candidates(candidates: &[String], encoding: &'static Encoding) -> Option<Vec<u8>> {
    let mut line = b"1/".to_vec();
    let mut any = false;
    for candidate in candidates {
        let Some(escaped) = escape_candidate(candidate) else {
            continue;
        };
        let (bytes, _, unmappable) = encoding.encode(&escaped);
        if unmappable {
            continue;
        }
        line.extend_from_slice(&bytes);
        line.push(b'/');
        any = true;
    }
    line.push(b'\n');
    any.then_some(line)
}

fn write_key_response<W: Write>(
    writer: &mut W,
    key: &[u8],
    candidates: &[String],
    encoding: &'static Encoding,
) -> io::Result<()> {
    if let Some(line) = encode_candidates(candidates, encoding) {
        writer.write_all(&line)
    } else {
        writer.write_all(b"4")?;
        writer.write_all(key)?;
        writer.write_all(b" \n")
    }
}

/// Serve requests until the peer disconnects.
pub fn serve<R, W, L>(
    reader: &mut R,
    writer: &mut W,
    backend: &L,
    encoding: &'static Encoding,
    version: &str,
    host: &str,
) -> io::Result<()>
where
    R: BufRead,
    W: Write,
    L: Lookup + ?Sized,
{
    let decode = |key: &[u8]| {
        encoding
            .decode_without_bom_handling_and_without_replacement(key)
            .map(std::borrow::Cow::into_owned)
    };
    while let Some(request) = read_request(reader)? {
        match request {
            Request::Disconnect => return Ok(()),
            Request::Version => writer.write_all(format!("{version} ").as_bytes())?,
            Request::Host => writer.write_all(format!("{host}: ").as_bytes())?,
            Request::Lookup(key) => {
                let candidates = decode(&key).map_or_else(Vec::new, |k| backend.lookup(&k));
                write_key_response(writer, &key, &candidates, encoding)?;
            }
            Request::Complete(key) => {
                let candidates = decode(&key).map_or_else(Vec::new, |k| backend.complete(&k));
                write_key_response(writer, &key, &candidates, encoding)?;
            }
        }
        writer.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::{EUC_JP, UTF_8};
    use std::io::Cursor;

    struct Fixed;
    impl Lookup for Fixed {
        fn lookup(&self, reading: &str) -> Vec<String> {
            match reading {
                "かんじ" => vec!["漢字".into(), "感じ".into()],
                "ぴえん" => vec!["🥺".into()],
                _ => vec![],
            }
        }
        fn complete(&self, prefix: &str) -> Vec<String> {
            if prefix == "かん" {
                vec!["かんじ".into()]
            } else {
                vec![]
            }
        }
    }

    fn run(input: &[u8], encoding: &'static Encoding) -> Vec<u8> {
        let mut reader = Cursor::new(input.to_vec());
        let mut output = Vec::new();
        serve(
            &mut reader,
            &mut output,
            &Fixed,
            encoding,
            "test.1",
            "h:127.0.0.1",
        )
        .unwrap();
        output
    }

    fn euc(text: &str) -> Vec<u8> {
        EUC_JP.encode(text).0.into_owned()
    }

    #[test]
    fn lookup_hit_in_euc_jp() {
        let mut request = b"1".to_vec();
        request.extend(euc("かんじ"));
        request.push(b' ');
        let mut expected = b"1/".to_vec();
        expected.extend(euc("漢字/感じ/"));
        expected.push(b'\n');
        assert_eq!(run(&request, EUC_JP), expected);
    }

    #[test]
    fn miss_echoes_key() {
        let key = euc("なし");
        let mut request = b"1".to_vec();
        request.extend(&key);
        request.push(b' ');
        let mut expected = b"4".to_vec();
        expected.extend(&key);
        expected.extend(b" \n");
        assert_eq!(run(&request, EUC_JP), expected);
    }

    #[test]
    fn unmappable_candidate_becomes_miss_in_euc_jp() {
        let mut request = b"1".to_vec();
        request.extend(euc("ぴえん"));
        request.push(b' ');
        assert_eq!(run(&request, EUC_JP)[0], b'4');
        let output = run("1ぴえん ".as_bytes(), UTF_8);
        assert_eq!(output, "1/🥺/\n".as_bytes());
    }

    #[test]
    fn persistent_connection_and_control_commands() {
        let output = run("2 3 1かんじ\n0 1かんじ ".as_bytes(), UTF_8);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "test.1 h:127.0.0.1: 1/漢字/感じ/\n"
        );
    }

    #[test]
    fn completion() {
        assert_eq!(run("4かん ".as_bytes(), UTF_8), "1/かんじ/\n".as_bytes());
    }

    #[test]
    fn oversized_key_is_rejected() {
        let mut request = b"1".to_vec();
        request.extend(vec![b'a'; MAX_KEY_BYTES + 1]);
        let mut reader = Cursor::new(request);
        assert!(read_request(&mut reader).is_err());
    }

    #[test]
    fn metacharacters_cannot_break_framing_or_eval() {
        assert_eq!(escape_candidate("a/b").unwrap(), r#"(concat "a\057b")"#);
        assert_eq!(escape_candidate("a;b").unwrap(), r#"(concat "a\073b")"#);
        assert_eq!(
            escape_candidate("(shell-command \"x\")").unwrap(),
            r#"(concat "\050shell-command \042x\042\051")"#
        );
        assert_eq!(escape_candidate("漢字").unwrap(), "漢字");
        assert_eq!(escape_candidate("a\nb"), None);
    }
}
