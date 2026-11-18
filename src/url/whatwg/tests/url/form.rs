use bun_engine_text::Text;
use bun_url_whatwg::SearchParams;
use bun_url_whatwg::form::{self, Pair};

use crate::{ascii, string, url};

fn pair(name: &str, value: &str) -> Pair {
    let (name, value) = (string(name), string(value));
    Pair { name, value }
}

fn params(init: &str) -> SearchParams {
    SearchParams::parse(string(init).text(), usize::MAX).unwrap()
}

fn serialized(params: &SearchParams) -> String {
    params.serialize().unwrap().to_string()
}

#[test]
fn parser() {
    let cases: [(&str, &[(&str, &str)]); 10] = [
        ("", &[]),
        ("&&", &[]),
        ("a&=&b=&=c", &[("a", ""), ("", ""), ("b", ""), ("", "c")]),
        ("a==b=&", &[("a", "=b=")]),
        ("+a+=%2B%20", &[(" a ", "+ ")]),
        ("%=%4&%zz%41", &[("%", "%4"), ("%zzA", "")]),
        ("é=%C3%A9é", &[("é", "éé")]),
        ("†=%E2%80&%EF%BB%BF", &[("†", "\u{fffd}"), ("\u{feff}", "")]),
        ("%C3©", &[("\u{fffd}©", "")]),
        ("?a= \t", &[("?a", " \t")]),
    ];
    for (input, expected) in cases {
        let expected: Vec<Pair> = expected.iter().map(|&(a, b)| pair(a, b)).collect();
        let wide: Vec<u16> = input.encode_utf16().collect();
        for text in [string(input).text(), Text::Utf16(&wide)] {
            assert_eq!(form::parse(text, usize::MAX).unwrap(), expected, "{input}");
        }
    }
}

#[test]
fn serializer() {
    let printable: String = (' '..='~').collect();
    let (name, value) = (string(&printable), [0xe9, 0xd800, 0xd83d, 0xde00]);
    let pairs = [
        (name.text(), Text::Utf16(&value)),
        (Text::EMPTY, Text::Latin1(b"\xe9\0")),
    ];
    assert_eq!(
        form::serialize(pairs).unwrap().to_string(),
        "+%21%22%23%24%25%26%27%28%29*%2B%2C-.%2F0123456789%3A%3B%3C%3D%3E%3F%40ABCDEFGHIJKLMNOPQRSTUVWXYZ%5B%5C%5D%5E_\
         %60abcdefghijklmnopqrstuvwxyz%7B%7C%7D%7E=%C3%A9%EF%BF%BD%F0%9F%98%80&=%C3%A9%00"
    );
}

#[test]
fn list() {
    let text = Text::from_ascii;
    let mut list = params("?b=1&a=2&b=3&b=1");
    assert_eq!(list.get_all(text("b")).count(), 3);
    assert!(list.has(text("b"), Some(text("3"))) && !list.has(text("a"), Some(text("3"))));
    list.delete(text("b"), Some(text("1")));
    assert_eq!(serialized(&list), "a=2&b=3");
    list.append(pair("a", "4"), usize::MAX).unwrap();
    list.set(pair("a", "5"), usize::MAX).unwrap();
    list.set(pair("c", "6"), usize::MAX).unwrap();
    assert_eq!(serialized(&list), "a=5&b=3&c=6");
    list.delete(text("b"), None);
    assert_eq!(list.pairs(), [pair("a", "5"), pair("c", "6")]);

    let mut unsorted = params("ﬃ&z=2&🌈&z=1&é");
    unsorted.sort();
    assert_eq!(unsorted, params("z=2&z=1&é&🌈&ﬃ"));

    let mut owner = url("http://h/#f");
    owner
        .set_query(Some(list.serialize().unwrap().text()))
        .unwrap();
    assert_eq!(ascii(owner.href()), "http://h/?a=5&c=6#f");
}

#[test]
fn limits() {
    let limit = 2 * size_of::<Pair>();
    let parse = |input: &str| SearchParams::parse(string(input).text(), limit);
    let mut full = parse("a&b").unwrap();
    assert!(full.append(pair("c", ""), limit).is_err() && full.set(pair("c", ""), limit).is_err());
    assert!(full.set(pair("a", "1"), limit).is_ok());
    assert!(parse("a&b&c").is_err());
    assert!(parse(&"é".repeat(limit / 4)).is_ok() && parse(&"é".repeat(limit / 4 + 1)).is_err());
    assert!(parse(&"a".repeat(limit + 1)).is_ok());
}
