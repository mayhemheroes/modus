// Copyright 2021 Sergey Mechtaev

// This file is part of Modus.

// Modus is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Modus is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Modus.  If not, see <https://www.gnu.org/licenses/>.

use nom::character::complete::line_ending;
use nom::character::complete::not_line_ending;
use nom::error::convert_error;
use std::fmt;
use std::str;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use crate::dockerfile;
use crate::logic;

#[derive(Clone, PartialEq, Debug)]
pub enum Expression {
    Literal(Literal),

    // An operator applied to an expression
    OperatorApplication(Box<Expression>, Operator),

    // A conjunction of expressions.
    ConjunctionList(Vec<Expression>),

    // A disjunction of expressions.
    DisjunctionList(Vec<Expression>),
}

impl Expression {
    /// Simplifies the expression tree by replacing singletons by that single value.
    fn prune(self) -> Self {
        match self {
            Self::ConjunctionList(es) => {
                let mut pruned: Vec<Expression> = es.into_iter().map(|e| e.prune()).collect();
                if pruned.len() == 1 {
                    pruned.remove(0) // `remove` to avoid clone
                } else {
                    Self::ConjunctionList(pruned)
                }
            }
            Self::DisjunctionList(es) => {
                let mut pruned: Vec<Expression> = es.into_iter().map(|e| e.prune()).collect();
                if pruned.len() == 1 {
                    pruned.remove(0)
                } else {
                    Self::DisjunctionList(pruned)
                }
            }
            Self::OperatorApplication(expr, op) => {
                Self::OperatorApplication(Box::new(expr.prune()), op)
            }
            expr => expr,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct ModusClause {
    pub head: Literal,
    // If None, this clause is a fact.
    pub body: Option<Expression>,
}

/// Used to generate a new unique literal.
/// The current format is "__replaced[id]", e.g. `__replaced1`.
/// This is not syntactically valid so no risk of a user using this predicate name.
static AVAILABLE_LITERAL_INDEX: AtomicU32 = AtomicU32::new(0);

impl From<&crate::modusfile::ModusClause> for Vec<logic::Clause> {
    /// Convert a ModusClause into one supported by the IR.
    /// It converts logical or/; into multiple rules, which should be equivalent.
    fn from(modus_clause: &crate::modusfile::ModusClause) -> Self {
        let mut clauses: Vec<logic::Clause> = Vec::new();

        // REVIEW: lots of cloning going on below, double check if this is necessary.
        match &modus_clause.body {
            Some(Expression::Literal(l)) => clauses.push(logic::Clause {
                head: modus_clause.head.clone(),
                body: vec![l.clone()],
            }),
            // ignores operators for now
            Some(Expression::OperatorApplication(expr, _)) => {
                clauses.extend(Self::from(&ModusClause {
                    head: modus_clause.head.clone(),
                    body: Some(*expr.clone()),
                }))
            }
            Some(Expression::ConjunctionList(exprs)) => {
                let mut curr_literals: Vec<Literal> = Vec::new();
                for expr in exprs {
                    match expr {
                        Expression::Literal(l) => curr_literals.push(l.clone()),
                        expr => {
                            // Create a new literal to represent this goal and recursively expand out the goal.
                            let new_literal = Literal {
                                predicate: format!(
                                    "__replaced{}",
                                    AVAILABLE_LITERAL_INDEX.fetch_add(1, Ordering::SeqCst)
                                )
                                .into(),
                                args: Vec::new(),
                            };
                            curr_literals.push(new_literal.clone());
                            clauses.extend(Self::from(&ModusClause {
                                head: new_literal,
                                body: Some(expr.clone()),
                            }))
                        }
                    }
                }
                clauses.push(logic::Clause {
                    head: modus_clause.head.clone(),
                    body: curr_literals,
                })
            }
            Some(Expression::DisjunctionList(exprs)) => {
                for expr in exprs {
                    if let Expression::Literal(l) = expr {
                        clauses.push(logic::Clause {
                            head: modus_clause.head.clone(),
                            body: vec![l.clone()],
                        });
                    } else {
                        // Create a new literal to represent this goal and recursively expand out the goal.
                        let new_literal = Literal {
                            predicate: format!(
                                "__replaced{}",
                                AVAILABLE_LITERAL_INDEX.fetch_add(1, Ordering::SeqCst)
                            )
                            .into(),
                            args: Vec::new(),
                        };
                        clauses.push(logic::Clause {
                            head: modus_clause.head.clone(),
                            body: vec![new_literal.clone()],
                        });
                        clauses.extend(Self::from(&ModusClause {
                            head: new_literal,
                            body: Some(expr.clone()),
                        }))
                    }
                }
            }
            None => clauses.push(logic::Clause {
                head: modus_clause.head.clone(),
                body: Vec::new(),
            }),
        }
        clauses
    }
}

type ModusTerm = logic::IRTerm;
type Literal = logic::Literal<ModusTerm>;
type Fact = ModusClause;
type Rule = ModusClause;
pub type Operator = Literal;

#[derive(Clone, PartialEq, Debug)]
pub struct Modusfile(pub Vec<ModusClause>);

#[derive(Clone, PartialEq, Debug)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
    pre_release: String,
    build: String,
}

impl str::FromStr for Modusfile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match parser::modusfile(s) {
            Result::Ok((_, o)) => Ok(o),
            Result::Err(nom::Err::Error(e) | nom::Err::Failure(e)) => {
                Result::Err(format!("{}", convert_error(s, e)))
            }
            _ => unimplemented!(),
        }
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expression::OperatorApplication(expr, op) => {
                write!(f, "({})::{}", expr.to_string(), op)
            }
            Expression::Literal(l) => write!(f, "{}", l.to_string()),
            Expression::ConjunctionList(exprs) => {
                let s = exprs
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<String>>()
                    .join(", ");
                write!(f, "{}", s)
            }
            Expression::DisjunctionList(exprs) => {
                let s = exprs
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<String>>()
                    .join("; ");
                write!(f, "{}", s)
            }
        }
    }
}

// could write a macro that generates these
impl From<Literal> for Expression {
    fn from(l: Literal) -> Self {
        Expression::Literal(l)
    }
}

impl str::FromStr for ModusClause {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match parser::modus_clause(s) {
            Result::Ok((_, o)) => Ok(o),
            Result::Err(e) => Result::Err(format!("{}", e)),
        }
    }
}

impl fmt::Display for ModusClause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(e) = &self.body {
            write!(f, "{} :- {}.", self.head, e.to_string(),)
        } else {
            write!(f, "{}.", self.head)
        }
    }
}

impl str::FromStr for Literal {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match logic::parser::literal(parser::modus_const, parser::modus_var)(s) {
            Result::Ok((_, o)) => Ok(o),
            Result::Err(e) => Result::Err(format!("{}", e)),
        }
    }
}

pub mod parser {
    use crate::logic::parser::{literal, literal_identifier, IResult};

    use super::*;

    use nom::bytes::complete::is_not;
    use nom::character::complete::multispace0;
    use nom::combinator::cut;
    use nom::error::context;
    use nom::multi::{fold_many0, separated_list1};
    use nom::{
        branch::alt,
        bytes::complete::tag,
        character::complete::space0,
        combinator::{eof, map, recognize},
        multi::{many0, separated_list0},
        sequence::{delimited, preceded, separated_pair, terminated},
    };

    fn comment(s: &str) -> IResult<&str, &str> {
        delimited(tag("#"), not_line_ending, line_ending)(s)
    }

    fn comments(s: &str) -> IResult<&str, Vec<&str>> {
        delimited(
            multispace0,
            separated_list0(multispace0, comment),
            multispace0,
        )(s)
    }

    fn head(i: &str) -> IResult<&str, Literal> {
        literal(modus_const, modus_var)(i)
    }

    fn expression_inner(i: &str) -> IResult<&str, Expression> {
        let lit_parser = map(literal(modus_const, modus_var), |lit| {
            Expression::Literal(lit)
        });
        // These inner expression parsers can fully recurse.
        let op_application_parser = map(
            separated_pair(
                delimited(tag("("), body, cut(tag(")"))),
                tag("::"),
                cut(literal(modus_const, modus_var)),
            ),
            |(expr, operator)| Expression::OperatorApplication(Box::new(expr), operator),
        );
        let parenthesized_expr = delimited(tag("("), body, cut(tag(")")));
        alt((lit_parser, op_application_parser, parenthesized_expr))(i)
    }

    fn body(i: &str) -> IResult<&str, Expression> {
        let comma_separated_exprs = map(
            separated_list1(delimited(comments, tag(","), comments), expression_inner),
            Expression::ConjunctionList,
        );
        let semi_separated_exprs = map(
            separated_list1(
                delimited(comments, tag(";"), comments),
                comma_separated_exprs,
            ),
            Expression::DisjunctionList,
        );
        // Parses the body as a semicolon separated list of comma separated inner expressions.
        // This resolves ambiguity by making commas/and higher precedence.
        let (i, expr) = preceded(comments, semi_separated_exprs)(i)?;
        Ok((i, expr.prune()))
    }

    fn fact(i: &str) -> IResult<&str, ModusClause> {
        // Custom definition of fact since datalog facts are normally "head :- ", but Moduslog
        // defines it as "head."
        map(terminated(head, tag(".")), |h| ModusClause {
            head: h,
            body: None,
        })(i)
    }

    fn rule(i: &str) -> IResult<&str, ModusClause> {
        map(
            separated_pair(
                head,
                delimited(space0, tag(":-"), multispace0),
                cut(terminated(body, tag("."))),
            ),
            |(head, body)| ModusClause {
                head,
                body: Some(body),
            },
        )(i)
    }

    /// Processes the given string, converting escape substrings into the proper characters.
    ///
    /// This also supports string continuation, This allows users to write strings like: "Hello, \
    ///                                                                                 World!"
    /// which is actually just "Hello, World!".
    fn process_raw_string(s: &str) -> String {
        let mut processed = String::new();

        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('"') => processed.push('"'),
                    Some('\\') => processed.push('\\'),
                    Some('n') => processed.push('\n'),
                    Some('r') => processed.push('\r'),
                    Some('t') => processed.push('\t'),
                    Some('0') => processed.push('\0'),
                    Some('\n') => {
                        // string continuation so we'll ignore whitespace till we get to a non-whitespace.
                        while let Some(c) = chars.peek() {
                            if !c.is_whitespace() {
                                break;
                            }
                            chars.next();
                        }
                    }
                    Some(c) => {
                        // leave it unchanged if we don't recognize the escape char
                        processed.push('\\');
                        processed.push(c);
                    }
                    None => panic!("given string ends with an escape character"),
                }
            } else {
                processed.push(c);
            }
        }
        processed
    }

    fn string_content(i: &str) -> IResult<&str, String> {
        let (a, b) = recognize(fold_many0(
            // Either an escaped double quote or anything that's not a double quote.
            // It should try the escaped double quote first.
            alt((tag("\\\""), is_not("\""))),
            || "".to_string(),
            |a, b| a.to_owned() + b,
        ))(i)?;
        let s = process_raw_string(b);
        Ok((a, s))
    }

    pub fn modus_const(i: &str) -> IResult<&str, String> {
        // TODO: Support proper f-strings, don't treat f-strings as const.
        delimited(alt((tag("\""), tag("f\""))), string_content, cut(tag("\"")))(i)
    }

    pub fn variable_identifier(i: &str) -> IResult<&str, &str> {
        literal_identifier(i)
    }

    pub fn modus_var(i: &str) -> IResult<&str, &str> {
        variable_identifier(i)
    }

    pub fn modus_clause(i: &str) -> IResult<&str, ModusClause> {
        context("modus_clause", alt((fact, rule)))(i)
    }

    pub fn modusfile(i: &str) -> IResult<&str, Modusfile> {
        map(
            terminated(
                many0(preceded(
                    many0(dockerfile::parser::ignored_line),
                    modus_clause,
                )),
                terminated(many0(dockerfile::parser::ignored_line), eof),
            ),
            Modusfile,
        )(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fact() {
        let l1 = Literal {
            predicate: logic::Predicate("l1".into()),
            args: Vec::new(),
        };
        let c = ModusClause {
            head: l1,
            body: None,
        };
        assert_eq!("l1.", c.to_string());
        assert_eq!(Ok(c), "l1.".parse());
    }

    #[test]
    fn rule() {
        let l1 = Literal {
            predicate: logic::Predicate("l1".into()),
            args: Vec::new(),
        };
        let l2 = Literal {
            predicate: logic::Predicate("l2".into()),
            args: Vec::new(),
        };
        let l3 = Literal {
            predicate: logic::Predicate("l3".into()),
            args: Vec::new(),
        };
        let c = Rule {
            head: l1,
            body: Expression::ConjunctionList(vec![l2.into(), l3.into()]).into(),
        };
        assert_eq!("l1 :- l2, l3.", c.to_string());
        assert_eq!(Ok(c.clone()), "l1 :- l2, l3.".parse());
        assert_eq!(Ok(c.clone()), "l1 :- l2,\n\tl3.".parse());
    }

    #[test]
    fn rule_with_or() {
        let l1: Literal = "l1".parse().unwrap();
        let l2: Literal = "l2".parse().unwrap();
        let c = Rule {
            head: "foo".parse().unwrap(),
            body: Expression::DisjunctionList(vec![l1.into(), l2.into()]).into(),
        };

        assert_eq!("foo :- l1; l2.", c.to_string());
        assert_eq!(Ok(c.clone()), "foo :- l1; l2.".parse());
    }

    #[test]
    fn rule_with_operator() {
        let foo = Literal {
            predicate: logic::Predicate("foo".into()),
            args: Vec::new(),
        };
        let a = Literal {
            predicate: logic::Predicate("a".into()),
            args: Vec::new(),
        };
        let b = Literal {
            predicate: logic::Predicate("b".into()),
            args: Vec::new(),
        };
        let merge = Operator {
            predicate: logic::Predicate("merge".into()),
            args: Vec::new(),
        };
        let r = Rule {
            head: foo,
            body: Expression::OperatorApplication(
                Expression::ConjunctionList(vec![a.into(), b.into()]).into(),
                merge,
            )
            .into(),
        };
        assert_eq!("foo :- (a, b)::merge.", r.to_string());
        assert_eq!(Ok(r.clone()), "foo :- (a, b)::merge.".parse());
    }

    #[test]
    fn modusclause_to_clause() {
        let foo = Literal {
            predicate: logic::Predicate("foo".into()),
            args: Vec::new(),
        };
        let a = Literal {
            predicate: logic::Predicate("a".into()),
            args: Vec::new(),
        };
        let b = Literal {
            predicate: logic::Predicate("b".into()),
            args: Vec::new(),
        };
        let merge = Operator {
            predicate: logic::Predicate("merge".into()),
            args: Vec::new(),
        };
        let r = Rule {
            head: foo,
            body: Expression::OperatorApplication(
                Expression::ConjunctionList(vec![a.into(), b.into()]).into(),
                merge,
            )
            .into(),
        };
        assert_eq!("foo :- (a, b)::merge.", r.to_string());

        // Convert to the simpler syntax
        let c: Vec<logic::Clause> = (&r).into();
        assert_eq!(1, c.len());
        assert_eq!("foo :- a, b", c[0].to_string());
    }

    #[test]
    fn modus_constant() {
        // Could use https://crates.io/crates/test_case if this pattern occurs often
        let inp1 = r#""Hello\nWorld""#;
        let inp2 = r#""Tabs\tare\tbetter\tthan\tspaces""#;
        let inp3 = r#""Testing \
                       multiline.""#;
        let (_, s1) = parser::modus_const(inp1).unwrap();
        let (_, s2) = parser::modus_const(inp2).unwrap();
        let (_, s3) = parser::modus_const(inp3).unwrap();

        assert_eq!(s1, "Hello\nWorld");
        assert_eq!(s2, "Tabs\tare\tbetter\tthan\tspaces");
        assert_eq!(s3, "Testing multiline.");
    }

    #[test]
    fn modus_expression() {
        let a: Literal = "a".parse().unwrap();
        let b: Literal = "b".parse().unwrap();
        let c: Literal = "c".parse().unwrap();
        let d: Literal = "d".parse().unwrap();

        let e1 = Expression::ConjunctionList(vec![Expression::Literal(a), Expression::Literal(b)]);
        let e2 = Expression::ConjunctionList(vec![Expression::Literal(c), Expression::Literal(d)]);

        let expr = Expression::DisjunctionList(vec![e1, e2]);

        let expr_str = "a, b; c, d";
        assert_eq!(expr_str, expr.to_string());
        let rule = format!("foo :- {}.", expr_str);
        assert_eq!(Ok(Some(expr)), rule.parse().map(|r: ModusClause| r.body));
    }
}
