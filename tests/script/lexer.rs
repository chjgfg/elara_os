use elara_os::script::lexer::*;
use std::{assert, assert_eq, matches, panic};

#[test]
fn quoting_and_offsets() {
    let tokens = lex("echo \"星澜 $HOME\" \\$USER; pwd").unwrap();
    assert_eq!(tokens.len(), 5);
    let Kind::Word(word) = &tokens[2].kind else {
        panic!("word");
    };
    assert_eq!(word.parts[0].quote, Quote::Literal);
    assert_eq!(tokens[3].kind, Kind::Operator(Op::Semi));
}
#[test]
fn operators_and_comments() {
    assert_eq!(lex("a#b #comment\n").unwrap().len(), 2);
    assert!(lex("\\\n#comment").unwrap().is_empty());
    assert!(lex("echo '$(x)'").is_ok());
    assert!(matches!(lex("x &").unwrap()[1].kind,Kind::Operator(Op::Background)));
    for input in ["$(x", "x <<y", "x |& y", "'unclosed", "x\\", "x\0"] {
        assert!(lex(input).is_err(), "{input}");
    }
}
#[test]
fn bounds_and_keyword_quotes() {
    assert!(lex(&"x".repeat(MAX_BYTES + 1)).is_err());
    assert!(lex(&"\n".repeat(MAX_TOKENS + 1)).is_err());
    assert!(matches!(&lex("'if'").unwrap()[0].kind, Kind::Word(word) if word.bare().is_none()));
}
#[test]
fn empty_quotes_and_adjacent_segments_are_preserved() {
    let tokens = lex("'' \"\" a\"b\"'c'").unwrap();
    assert_eq!(tokens.len(), 3);
    let Kind::Word(word) = &tokens[0].kind else {
        panic!("word");
    };
    assert_eq!(word.parts[0].text, "");
    let Kind::Word(word) = &tokens[2].kind else {
        panic!("word");
    };
    assert_eq!(word.parts.len(), 3);
}
#[test]
fn literal_dollars_and_double_quote_escapes_keep_their_semantics() {
    let tokens = lex("echo \"\\$X\\q\"").unwrap();
    let Kind::Word(word) = &tokens[1].kind else {
        panic!("word");
    };
    assert!(word
        .parts
        .iter()
        .any(|p| p.quote == Quote::Literal && p.text == "$"));
    assert!(word
        .parts
        .iter()
        .any(|p| p.quote == Quote::Double && p.text == "X\\q"));
}
