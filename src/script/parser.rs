//! Recursive descent over a bounded token stream. No effects until the entire AST validates.
use super::lexer::{self, Error, Kind, Op, Token, Word};
use std::{matches, vec};

pub type List = Vec<Node>;
#[derive(Debug, Clone)]
pub enum Node {
    Background {
        body: Box<Node>,
    },
    Chain {
        first: Pipeline,
        rest: Vec<(Join, Pipeline)>,
    },
    If {
        branches: Vec<(List, List)>,
        otherwise: List,
    },
    For {
        name: String,
        items: Option<Vec<Word>>,
        body: List,
    },
    While {
        condition: List,
        body: List,
        until: bool,
    },
    Case {
        word: Word,
        branches: Vec<(Vec<Word>, List)>,
    },
    Function {
        name: String,
        body: List,
    },
}
#[derive(Debug, Clone, Copy)]
pub enum Join {
    And,
    Or,
}
#[derive(Debug, Clone)]
pub struct Pipeline {
    pub commands: Vec<Command>,
}
#[derive(Debug, Clone)]
pub struct Command {
    pub words: Vec<Word>,
    pub redirects: Vec<Redirect>,
}
#[derive(Debug, Clone)]
pub struct Redirect {
    pub op: Op,
    pub path: Word,
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    depth: usize,
    expansion_depth: usize,
}
impl Parser {
    fn error(&self, message: &str) -> Error {
        Error::new(self.tokens.get(self.at).map_or(0, |t| t.byte), message)
    }
    fn op(&self, op: Op) -> bool {
        self.tokens
            .get(self.at)
            .is_some_and(|t| t.kind == Kind::Operator(op))
    }
    fn bare(&self) -> Option<&str> {
        match &self.tokens.get(self.at)?.kind {
            Kind::Word(w) => w.bare(),
            _ => None,
        }
    }
    fn keyword(&mut self, word: &str) -> Result<(), Error> {
        if self.bare() != Some(word) {
            return Err(self.error(word));
        }
        self.at += 1;
        Ok(())
    }
    fn newlines(&mut self) {
        while self.op(Op::Newline) {
            self.at += 1;
        }
    }
    fn separators(&mut self) {
        while self.op(Op::Newline) || self.op(Op::Semi) {
            self.at += 1;
        }
    }
    fn word(&mut self) -> Result<Word, Error> {
        let Some(Token {
            kind: Kind::Word(word),
            byte,
            ..
        }) = self.tokens.get(self.at)
        else {
            return Err(self.error("word expected"));
        };
        lexer::validate_at_depth(word, self.expansion_depth)
            .map_err(|error| Error::new(*byte, error.message))?;
        let word = word.clone();
        self.at += 1;
        Ok(word)
    }
    fn list(&mut self, stops: &[&str], close: bool) -> Result<List, Error> {
        if self.depth >= 32 {
            return Err(self.error("syntax nesting exceeds 32"));
        }
        self.depth += 1;
        let result = self.list_inner(stops, close);
        self.depth -= 1;
        result
    }
    fn list_inner(&mut self, stops: &[&str], close: bool) -> Result<List, Error> {
        let mut nodes = Vec::new();
        self.separators();
        while !(self.at >= self.tokens.len()
            || self.op(Op::DoubleSemi)
            || self.bare().is_some_and(|w| stops.contains(&w))
            || close && self.op(Op::Close))
        {
            let node = self.node()?;
            if self.op(Op::Background) {
                self.at += 1;
                nodes.push(Node::Background {
                    body: Box::new(node),
                });
                self.separators();
                continue;
            }
            nodes.push(node);
            if self.at == self.tokens.len() || self.op(Op::DoubleSemi) {
                break;
            }
            if !self.op(Op::Newline) && !self.op(Op::Semi) {
                return Err(self.error("command separator expected"));
            }
            self.separators();
        }
        Ok(nodes)
    }
    fn node(&mut self) -> Result<Node, Error> {
        match self.bare() {
            Some("case") => return self.case_node(),
            Some("if") => return self.if_node(),
            Some("for") => return self.for_node(),
            Some("while") | Some("until") => {
                let until = self.bare() == Some("until");
                self.at += 1;
                let condition = self.list(&["do"], false)?;
                if condition.is_empty() {
                    return Err(self.error("empty loop condition"));
                }
                self.keyword("do")?;
                let body = self.list(&["done"], false)?;
                self.keyword("done")?;
                return Ok(Node::While {
                    condition,
                    body,
                    until,
                });
            }
            Some("then" | "else" | "elif" | "fi" | "do" | "done" | "in" | "esac") => {
                return Err(self.error("unexpected reserved word"))
            }
            _ => {}
        }
        if self
            .tokens
            .get(self.at + 1)
            .is_some_and(|t| t.kind == Kind::Operator(Op::Left))
        {
            let name = self
                .bare()
                .filter(|name| lexer::valid_name(name))
                .ok_or_else(|| self.error("invalid function name"))?
                .to_owned();
            self.at += 2;
            if !self.op(Op::Right) {
                return Err(self.error("function requires ()"));
            }
            self.at += 1;
            self.newlines();
            if !self.op(Op::Open) {
                return Err(self.error("function requires a brace body"));
            }
            self.at += 1;
            let body = self.list(&[], true)?;
            if !self.op(Op::Close) {
                return Err(self.error("missing function closing brace"));
            }
            self.at += 1;
            return Ok(Node::Function { name, body });
        }
        let first = self.pipeline()?;
        let mut rest = Vec::new();
        while self.op(Op::And) || self.op(Op::Or) {
            let join = if self.op(Op::And) {
                Join::And
            } else {
                Join::Or
            };
            self.at += 1;
            self.newlines();
            rest.push((join, self.pipeline()?));
        }
        Ok(Node::Chain { first, rest })
    }
    fn case_node(&mut self) -> Result<Node, Error> {
        self.keyword("case")?;
        let word = self.word()?;
        self.newlines();
        self.keyword("in")?;
        self.newlines();
        let mut branches = Vec::new();
        while self.bare() != Some("esac") {
            if self.op(Op::Left) {
                self.at += 1;
            }
            let mut patterns = vec![self.word()?];
            while self.op(Op::Pipe) {
                self.at += 1;
                patterns.push(self.word()?);
            }
            if !self.op(Op::Right) {
                return Err(self.error("case pattern requires )"));
            }
            self.at += 1;
            let body = self.list(&["esac"], false)?;
            branches.push((patterns, body));
            if self.op(Op::DoubleSemi) {
                self.at += 1;
                self.newlines();
            } else if self.bare() != Some("esac") {
                return Err(self.error("case branch requires ;;"));
            }
        }
        self.keyword("esac")?;
        Ok(Node::Case { word, branches })
    }
    fn if_node(&mut self) -> Result<Node, Error> {
        self.keyword("if")?;
        let mut branches = Vec::new();
        loop {
            let condition = self.list(&["then"], false)?;
            if condition.is_empty() {
                return Err(self.error("empty if condition"));
            }
            self.keyword("then")?;
            let body = self.list(&["elif", "else", "fi"], false)?;
            branches.push((condition, body));
            if self.bare() == Some("elif") {
                self.at += 1;
            } else {
                break;
            }
        }
        let otherwise = if self.bare() == Some("else") {
            self.at += 1;
            self.list(&["fi"], false)?
        } else {
            Vec::new()
        };
        self.keyword("fi")?;
        Ok(Node::If {
            branches,
            otherwise,
        })
    }
    fn for_node(&mut self) -> Result<Node, Error> {
        self.keyword("for")?;
        let name = self
            .bare()
            .filter(|n| lexer::valid_name(n))
            .ok_or_else(|| self.error("invalid loop variable"))?
            .to_owned();
        self.at += 1;
        let items = if self.bare() == Some("in") {
            self.at += 1;
            let mut words = Vec::new();
            while self
                .tokens
                .get(self.at)
                .is_some_and(|t| matches!(t.kind, Kind::Word(_)))
            {
                words.push(self.word()?);
            }
            Some(words)
        } else {
            None
        };
        if !self.op(Op::Semi) && !self.op(Op::Newline) {
            return Err(self.error("for requires a separator before do"));
        }
        self.separators();
        self.keyword("do")?;
        let body = self.list(&["done"], false)?;
        self.keyword("done")?;
        Ok(Node::For { name, items, body })
    }
    fn pipeline(&mut self) -> Result<Pipeline, Error> {
        let mut commands = Vec::new();
        commands.push(self.command()?);
        while self.op(Op::Pipe) {
            self.at += 1;
            self.newlines();
            commands.push(self.command()?);
        }
        Ok(Pipeline { commands })
    }
    fn command(&mut self) -> Result<Command, Error> {
        let mut words = Vec::new();
        let mut redirects = Vec::new();
        let mut input = false;
        let mut output = false;
        while let Some(token) = self.tokens.get(self.at) {
            match &token.kind {
                Kind::Word(word) => {
                    if word
                        .bare()
                        .is_some_and(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()))
                        && self.tokens.get(self.at + 1).is_some_and(|t| {
                            t.byte == token.end
                                && matches!(
                                    t.kind,
                                    Kind::Operator(Op::Input | Op::Output | Op::Append)
                                )
                        })
                    {
                        return Err(self.error("file-descriptor redirection is not supported"));
                    }
                    words.push(self.word()?);
                }
                Kind::Operator(op @ (Op::Input | Op::Output | Op::Append)) => {
                    let op = *op;
                    let seen = if op == Op::Input {
                        &mut input
                    } else {
                        &mut output
                    };
                    if *seen {
                        return Err(self
                            .error("one input and one output redirect per command are supported"));
                    }
                    *seen = true;
                    self.at += 1;
                    let path = self.word()?;
                    redirects.push(Redirect { op, path });
                }
                _ => break,
            }
        }
        if words.is_empty() {
            return Err(self.error("command expected"));
        }
        Ok(Command { words, redirects })
    }
}

pub fn parse(source: &str) -> Result<List, Error> {
    parse_with_depth(source, 0)
}
pub(super) fn parse_with_depth(source: &str, expansion_depth: usize) -> Result<List, Error> {
    if expansion_depth >= 32 {
        return Err(Error::new(0, "expansion nesting exceeds 32"));
    }
    if source.len() > lexer::MAX_BYTES {
        return Err(Error::new(lexer::MAX_BYTES, "script exceeds 64 KiB"));
    }
    // Normalize Windows line endings deliberately; error byte offsets refer to this normalized text.
    let source = source.replace("\r\n", "\n");
    let mut parser = Parser {
        tokens: lexer::lex(&source)?,
        at: 0,
        depth: 0,
        expansion_depth,
    };
    let list = parser.list(&[], false)?;
    if parser.at != parser.tokens.len() {
        return Err(parser.error("unexpected token"));
    }
    Ok(list)
}
