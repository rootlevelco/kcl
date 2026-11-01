//! KCL indent handling.

use std::cmp::Ordering;

use crate::lexer::IndentOrDedents;
use crate::lexer::Lexer;
use crate::lexer::TokenStreamBuilder;
use kcl_ast::token::VALID_SPACES_LENGTH;
use kcl_ast::token::{self, Token};

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) struct IndentLevel {
    pub(crate) tabs: usize,
    pub(crate) spaces: usize,
    /// It came from a line that goes on with the one before, so no indent
    /// token stands for it and no dedent closes it.
    pub(crate) continued: bool,
}

impl IndentLevel {
    pub(crate) fn cmp(&self, other: &IndentLevel) -> Result<Ordering, &'static str> {
        match self.tabs.cmp(&other.tabs) {
            Ordering::Less => {
                if self.spaces <= other.spaces {
                    Ok(Ordering::Less)
                } else {
                    Err("inconsistent use of tabs and spaces in indentation")
                }
            }
            Ordering::Greater => {
                if self.spaces >= other.spaces {
                    Ok(Ordering::Greater)
                } else {
                    Err("inconsistent use of tabs and spaces in indentation")
                }
            }
            Ordering::Equal => Ok(self.spaces.cmp(&other.spaces)),
        }
    }
}

impl<'a> Lexer<'a> {
    pub(crate) fn lex_indent_context(
        &mut self,
        token: kcl_lexer::TokenKind,
        buf: &mut TokenStreamBuilder,
    ) -> Option<IndentOrDedents> {
        // process for indent context for a newline
        if !self.indent_cxt.new_line_beginning {
            return None;
        }

        match token {
            kcl_lexer::TokenKind::LineComment { doc_style: _ } | kcl_lexer::TokenKind::Newline => {
                // No in(de)ent in comment line and new line
                self.indent_cxt.tabs = 0;
                self.indent_cxt.spaces = 0;
                None
            }
            kcl_lexer::TokenKind::Tab => {
                self.indent_cxt.tabs += 1;
                None
            }
            kcl_lexer::TokenKind::Space => {
                self.indent_cxt.spaces += 1;
                None
            }
            _ => {
                // End of detect on unrelated token, then do lex indent.
                self.indent_cxt.new_line_beginning = false;
                if matches!(token, kcl_lexer::TokenKind::Or) && self.continue_line(buf) {
                    return None;
                }
                self.lex_indent()
            }
        }
    }

    /// A line starting with `|`, indented deeper than the line before it or
    /// level with another such line, goes on with that line. Nothing else
    /// valid can start an indented line after a finished expression. It
    /// lexes as if the line break weren't there.
    fn continue_line(&mut self, buf: &mut TokenStreamBuilder) -> bool {
        let level = IndentLevel {
            tabs: self.indent_cxt.tabs,
            spaces: self.indent_cxt.spaces,
            continued: true,
        };
        let last = *self.last_indent();
        match level.cmp(&last) {
            Ok(Ordering::Greater) => self.indent_cxt.indents.push(level),
            Ok(Ordering::Equal) if last.continued => {}
            _ => return false,
        }
        self.indent_cxt.tabs = 0;
        self.indent_cxt.spaces = 0;
        buf.drop_line_end();
        true
    }

    fn lex_indent(&mut self) -> Option<IndentOrDedents> {
        let tabs = self.indent_cxt.tabs;
        let spaces = self.indent_cxt.spaces;
        // reset counters
        self.indent_cxt.tabs = 0;
        self.indent_cxt.spaces = 0;

        // process indent at the end of the newline
        let mut cur_indent = *self.last_indent();
        let indent = IndentLevel {
            tabs,
            spaces,
            continued: false,
        };
        let mut ordering = indent.cmp(&cur_indent);
        // Level with `|` lines but not one: an indented line of its own.
        if cur_indent.continued
            && ordering == Ok(Ordering::Equal)
            && self.indent_cxt.delims.is_empty()
        {
            self.indent_cxt.indents.pop();
            self.indent_cxt.indents.push(indent);
            let token = Token::new(
                token::Indent(VALID_SPACES_LENGTH),
                self.span(self.pos, self.pos),
            );
            return Some(IndentOrDedents::Indent { token });
        }

        match ordering {
            Ok(order) => {
                Some(match order {
                    Ordering::Greater => {
                        self.indent_cxt.indents.push(indent);

                        // For indent token, we ignore the length
                        let indent = Token::new(
                            token::Indent(VALID_SPACES_LENGTH),
                            self.span(self.pos, self.pos),
                        );
                        IndentOrDedents::Indent { token: indent }
                    }
                    Ordering::Less => {
                        let mut dedents = Vec::new();
                        let mut indents = Vec::new();

                        loop {
                            match ordering {
                                Ok(order) => {
                                    match order {
                                        Ordering::Less => {
                                            // Pop indents util we find an equal ident level
                                            let popped = self.indent_cxt.indents.pop();
                                            let continued = popped.is_some_and(|i| i.continued);
                                            if let Some(indent) = popped {
                                                indents.push(indent);
                                            }
                                            // update pos & collect dedent
                                            // For dedent token, we ignore the length
                                            let dedent = Token::new(
                                                token::Dedent(VALID_SPACES_LENGTH),
                                                self.span(self.pos, self.pos),
                                            );
                                            if !continued {
                                                dedents.push(dedent);
                                            }
                                        }
                                        Ordering::Equal => {
                                            // Proper indent level found.
                                            break;
                                        }
                                        Ordering::Greater => {
                                            let spaces_diff = indent.spaces - cur_indent.spaces;
                                            if let Some(indent) = indents.pop() {
                                                self.indent_cxt.indents.push(indent);
                                            }
                                            dedents.pop();
                                            dedents.push(Token::new(
                                                token::Dedent(spaces_diff),
                                                self.span(self.pos, self.pos),
                                            ));
                                            break;
                                        }
                                    }

                                    // update cur indent and ordering
                                    cur_indent = *self.last_indent();
                                    ordering = indent.cmp(&cur_indent);
                                }
                                Err(msg) => {
                                    self.sess
                                        .struct_span_error(msg, self.span(self.pos, self.pos));
                                    break;
                                }
                            }
                        }
                        IndentOrDedents::Dedents { tokens: dedents }
                    }
                    _ => return None,
                })
            }
            Err(msg) => {
                self.sess
                    .struct_span_error(msg, self.span(self.pos, self.pos));
                None
            }
        }
    }

    /// Get the last indent, if not exists, return a default level for error recovery.
    fn last_indent(&mut self) -> &IndentLevel {
        if self.indent_cxt.indents.is_empty() {
            self.sess
                .struct_span_error("mismatched indent level", self.span(self.pos, self.pos));
            self.indent_cxt.indents.push(IndentLevel::default());
        }
        self.indent_cxt.indents.last().unwrap()
    }
}
