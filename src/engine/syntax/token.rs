//! What the lexer hands to the parser.

use crate::names::Name;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub(crate) enum TokenKind {
    Identifier,
    // A keyword is the index of its `Name`.
    Null,
    True,
    False,
    Break,
    Case,
    Default,
    For,
    New,
    Var,
    Const,
    Continue,
    Function,
    Return,
    If,
    This,
    Do,
    While,
    Switch,
    With,
    Throw,
    Try,
    Catch,
    Finally,
    Debugger,
    Else,
    Import,
    Export,
    Class,
    Extends,
    Super,
    Typeof,
    Void,
    Delete,
    Instanceof,
    In,
    Enum,
    Let,
    Yield,
    Await,
    /// In code that is not strict these are identifiers.
    ReservedInStrictCode,

    EscapedKeyword,
    PrivateName,
    Number,
    BigInt,
    String,
    Template,
    RegExp,

    // No expression goes on after these.
    Comma,
    Semicolon,
    Colon,
    CloseParen,
    CloseBracket,
    CloseBrace,
    End,

    // These go on with a member expression.
    Dot,
    OpenParen,
    OpenBracket,
    Backquote,
    QuestionDot,

    OpenBrace,
    Question,
    Ellipsis,
    Arrow,

    Assign,
    AddAssign,
    SubtractAssign,
    MultiplyAssign,
    DivideAssign,
    RemainderAssign,
    ExponentiateAssign,
    BitAndAssign,
    BitOrAssign,
    BitXorAssign,
    ShiftLeftAssign,
    ShiftRightAssign,
    ShiftRightUnsignedAssign,
    AndAssign,
    OrAssign,
    CoalesceAssign,

    Increment,
    Decrement,
    Not,
    BitNot,
    Plus,
    Minus,

    Star,
    Slash,
    Percent,
    Exponentiate,
    BitAnd,
    BitOr,
    BitXor,
    ShiftLeft,
    ShiftRight,
    ShiftRightUnsigned,
    Equal,
    NotEqual,
    StrictEqual,
    StrictNotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
    Coalesce,

    // Names that are malformed.
    IncompleteEscape,
    InvalidEscape,
    InvalidUnicodeEscape,
    InvalidEncoding,
    InvalidCodePoint,
    // `Lexer::error` says more of these.
    Invalid,
    Unterminated,
    /// What more lines could complete.
    UnterminatedMultiline,
}

const KIND_COUNT: usize = TokenKind::UnterminatedMultiline as usize + 1;

/// By the index of a name, up to the last of those that strict code reserves.
static KEYWORDS: [TokenKind; 64] = {
    use TokenKind::*;
    let keywords = [
        Identifier, Null, True, False, Break, Case, Default, For, New, Var, Const, Continue,
        Function, Return, If, This, Do, While, Switch, With, Throw, Try, Catch, Finally, Debugger,
        Else, Import, Export, Class, Extends, Super, Typeof, Void, Delete, Instanceof, In, Enum,
        Let, Yield, Await,
    ];
    assert!(keywords.len() == Name::IMPLEMENTS.index() && Name::STATIC.index() < 64);
    let mut kinds = [ReservedInStrictCode; 64];
    let mut index = 0;
    while index < keywords.len() {
        assert!(keywords[index] as usize == index);
        kinds[index] = keywords[index];
        index += 1;
    }
    kinds
};

pub(crate) const RELATIONAL_PRECEDENCE: u8 = 8;

static PRECEDENCES: [u8; KIND_COUNT] = {
    use TokenKind::*;
    let levels: [&[TokenKind]; 12] = [
        &[Coalesce],
        &[Or],
        &[And],
        &[BitOr],
        &[BitXor],
        &[BitAnd],
        &[Equal, NotEqual, StrictEqual, StrictNotEqual],
        &[Less, LessEqual, Greater, GreaterEqual, Instanceof, In],
        &[ShiftLeft, ShiftRight, ShiftRightUnsigned],
        &[Plus, Minus],
        &[Star, Slash, Percent],
        &[Exponentiate],
    ];
    let mut precedences = [0; KIND_COUNT];
    let mut level = 0;
    while level < levels.len() {
        let mut index = 0;
        while index < levels[level].len() {
            precedences[levels[level][index] as usize] = level as u8 + 1;
            index += 1;
        }
        level += 1;
    }
    assert!(precedences[In as usize] == RELATIONAL_PRECEDENCE);
    precedences
};

impl TokenKind {
    /// `last_keyword` is the index of the last name that is reserved.
    #[inline(always)]
    pub(crate) fn of_name(name: Name, last_keyword: u32) -> TokenKind {
        let index = name.index() as u32;
        let index = if index <= last_keyword { index } else { 0 };
        KEYWORDS[index as usize % KEYWORDS.len()]
    }

    #[inline(always)]
    fn is_in(self, first: TokenKind, last: TokenKind) -> bool {
        (self as u8).wrapping_sub(first as u8) <= last as u8 - first as u8
    }

    #[inline(always)]
    pub(crate) fn is_keyword(self) -> bool {
        self.is_in(TokenKind::Null, TokenKind::ReservedInStrictCode)
    }

    /// IdentifierName of ECMA-262.
    #[inline(always)]
    pub(crate) fn is_identifier_name(self) -> bool {
        self <= TokenKind::EscapedKeyword
    }

    #[inline(always)]
    pub(crate) fn ends_expression(self) -> bool {
        self.is_in(TokenKind::Comma, TokenKind::End)
    }

    #[inline(always)]
    pub(crate) fn continues_member(self) -> bool {
        self.is_in(TokenKind::Dot, TokenKind::QuestionDot)
    }

    #[inline(always)]
    pub(crate) fn is_assignment(self) -> bool {
        self.is_in(TokenKind::Assign, TokenKind::CoalesceAssign)
    }

    #[inline(always)]
    pub(crate) fn is_logical_assignment(self) -> bool {
        self.is_in(TokenKind::AndAssign, TokenKind::CoalesceAssign)
    }

    #[inline(always)]
    pub(crate) fn is_update(self) -> bool {
        self.is_in(TokenKind::Increment, TokenKind::Decrement)
    }

    #[inline(always)]
    pub(crate) fn is_prefix_operator(self) -> bool {
        self.is_in(TokenKind::Increment, TokenKind::Minus)
            || self.is_in(TokenKind::Typeof, TokenKind::Delete)
    }

    /// Of a binary operator. Zero for anything else.
    #[inline(always)]
    pub(crate) fn precedence(self) -> u8 {
        PRECEDENCES[self as usize]
    }

    /// Whether the token is all there is to say about an error at it.
    pub(crate) fn speaks_for_itself(self) -> bool {
        matches!(self, TokenKind::End | TokenKind::EscapedKeyword)
            || self >= TokenKind::IncompleteEscape
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Token {
    pub(crate) start: u32,
    pub(crate) end: u32,
    /// Of a name, its [`Name`]. Of a number that is not `is_elaborate`, the number.
    pub(crate) value: u32,
    pub(crate) kind: TokenKind,
    pub(crate) follows_newline: bool,
    /// The value is not the text as it stands: there are escapes, or more to a number than a few decimal digits.
    pub(crate) is_elaborate: bool,
}

impl Token {
    #[inline(always)]
    pub(crate) fn name(self) -> Name {
        Name::from_word(self.value).unwrap_or(Name::EMPTY)
    }

    /// An identifier that is a keyword in some places, where it cannot have escapes.
    #[inline(always)]
    pub(crate) fn is_contextual(self, name: Name) -> bool {
        self.kind == TokenKind::Identifier
            && self.value == name.index() as u32
            && !self.is_elaborate
    }

    /// `let`, `yield` or `await`, with or without escapes.
    #[inline(always)]
    pub(crate) fn spells(self, keyword: TokenKind) -> bool {
        self.kind == keyword
            || (self.kind == TokenKind::EscapedKeyword && self.value == keyword as u32)
    }
}
