// Copyright (C) 2022 Parity Technologies (UK) Ltd. (admin@parity.io)
// This file is a part of the scale-value crate.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//         http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![allow(clippy::enum_variant_names)]

use super::string_helpers;
use crate::value::{BitSequence, Composite, Primitive, Value, Variant};
use std::num::ParseIntError;
use yap::{IntoTokens, TokenLocation, Tokens};

pub fn from_str(s: &str) -> (Result<Value<()>, ParseError>, &str) {
	let mut toks = s.into_tokens();
	let res = parse_value(&mut toks);
	let remaining = toks.remaining();
	(res, remaining)
}

/// An error parsing the provided string into a Value
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub struct ParseError {
	/// Byte offset into the provided string that the error begins.
	pub start_loc: usize,
	/// Byte offset into the provided string that the error ends. Many errors begin at some
	/// point but do not have a known end position.
	pub end_loc: Option<usize>,
	/// Details about the error that occurred.
	pub err: ParseErrorKind,
}

impl ParseError {
	fn new_at(err: ParseErrorKind, loc: usize) -> Self {
		Self { start_loc: loc, end_loc: None, err }
	}
	fn new_between(err: ParseErrorKind, start: usize, end: usize) -> Self {
		Self { start_loc: start, end_loc: Some(end), err }
	}
}

impl std::fmt::Display for ParseError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		if let Some(end_loc) = self.end_loc {
			write!(f, "Error from char {} to {}: {}", self.start_loc, end_loc, self.err)
		} else {
			write!(f, "Error at char {}: {}", self.start_loc, self.err)
		}
	}
}

/// Details about the error that occurred.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseErrorKind {
	#[error("Expected a value")]
	ExpectedValue,
	#[error("{0}")]
	Complex(#[from] ParseComplexError),
	#[error("{0}")]
	Char(#[from] ParseCharError),
	#[error("{0}")]
	String(#[from] ParseStringError),
	#[error("{0}")]
	Number(#[from] ParseNumberError),
	#[error("{0}")]
	BitSequence(#[from] ParseBitSequenceError),
}

// Add handy helper methods to sub-error-kinds
macro_rules! at_between {
	($ty:ident) => {
		impl $ty {
			/// Error at a specific location with no specific end
			#[allow(unused)]
			fn at(self, loc: usize) -> ParseError {
				let e: ParseErrorKind = self.into();
				ParseError::new_at(e, loc)
			}
			/// Error at a specific location for the next character
			#[allow(unused)]
			fn at_one(self, loc: usize) -> ParseError {
				let e: ParseErrorKind = self.into();
				ParseError::new_between(e, loc, loc + 1)
			}
			/// Error between two locations.
			#[allow(unused)]
			fn between(self, start: usize, end: usize) -> ParseError {
				let e: ParseErrorKind = self.into();
				ParseError::new_between(e, start, end)
			}
		}
	};
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseComplexError {
	#[error("The first character in a field name should be alphabetic")]
	InvalidStartingCharacterInIdent,
	#[error("Field name is not valid (it should begin with an alphabetical char and then consist only of alphanumeric chars)")]
	InvalidFieldName,
	#[error("Missing field separator; expected {0}")]
	MissingFieldSeparator(char),
	#[error("Missing closing '{0}'")]
	ExpectedCloserToMatch(char, usize),
}
at_between!(ParseComplexError);

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseCharError {
	#[error("Expected a single character")]
	ExpectedValidCharacter,
	#[error("Expected an escape code to follow the '\\'")]
	ExpectedValidEscapeCode,
	#[error("Expected a closing quote to match the opening quote at position {0}")]
	ExpectedClosingQuoteToMatch(usize),
}
at_between!(ParseCharError);

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseStringError {
	#[error("Expected a closing quote to match the opening quote at position {0}")]
	ExpectedClosingQuoteToMatch(usize),
	#[error("Expected an escape code to follow the '\\'")]
	ExpectedValidEscapeCode,
}
at_between!(ParseStringError);

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseNumberError {
	#[error("Expected one or more digits")]
	ExpectedDigit,
	#[error("Failed to parse digits into an integer: {0}")]
	ParsingFailed(ParseIntError),
}
at_between!(ParseNumberError);

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseBitSequenceError {
	#[error("Expected a closing bracket ('>') to match the opening one at position {0}")]
	ExpectedClosingBracketToMatch(usize),
	#[error("Invalid character; expecting a 0 or 1")]
	InvalidCharacter,
}
at_between!(ParseBitSequenceError);

// Parse a value.
fn parse_value(t: &mut impl Tokens<Item = char>) -> Result<Value<()>, ParseError> {
	// Our parsers return `Result<Thing, Option<ParseError>>`, but in order to know
	// whether to try the next item, `one_of` expects `Option<T>`, so we transpose_err
	// to convert to the right shape.
	let val = yap::one_of!(t;
		transpose_err(parse_bool(t).map(Value::bool).ok_or(None)),
		transpose_err(parse_char(t).map(Value::char)),
		transpose_err(parse_string(t).map(Value::string)),
		transpose_err(parse_number(t).map(Value::primitive)),
		transpose_err(parse_named_composite(t).map(|v| v.into())),
		transpose_err(parse_unnamed_composite(t).map(|v| v.into())),
		transpose_err(parse_bit_sequence(t).map(Value::bit_sequence)),
		transpose_err(parse_variant(t).map(|v| v.into())),
	);

	match val {
		Some(Ok(val)) => Ok(val),
		Some(Err(e)) => Err(e),
		None => Err(ParseError::new_at(ParseErrorKind::ExpectedValue, t.offset())),
	}
}

// Parse a named composite value like `{ foo: 123 }`.
//
// As with most of the parsers here, the error is optional. A `Some` error indicates that
// we're midway through parsing something and have run into an error. a `None` error indicates
// that we can see up front that the characters we're parsing aren't going to be the right shape,
// and can attempt to parse the characters into a different thing if we wish.
fn parse_named_composite(
	t: &mut impl Tokens<Item = char>,
) -> Result<Composite<()>, Option<ParseError>> {
	let start = t.offset();
	if !t.token('{') {
		return Err(None);
	}
	skip_whitespace(t);

	// No values? bail early.
	if t.token('}') {
		return Ok(Composite::Named(vec![]));
	}

	let vals = t
		.sep_by_err(|t| parse_field_name_and_value(t), |t| skip_spaced_separator(t, ','))
		.collect::<Result<_, _>>()?;

	skip_whitespace(t);
	if !t.token('}') {
		return Err(Some(ParseComplexError::ExpectedCloserToMatch('}', start).at_one(t.offset())));
	}
	Ok(Composite::Named(vals))
}

// Parse an unnamed composite value like `(true, 123)`
fn parse_unnamed_composite(
	t: &mut impl Tokens<Item = char>,
) -> Result<Composite<()>, Option<ParseError>> {
	let start = t.offset();
	if !t.token('(') {
		return Err(None);
	}
	skip_whitespace(t);

	// No values? bail early.
	if t.token(')') {
		return Ok(Composite::Unnamed(vec![]));
	}

	let vals = t
		.sep_by_err(|t| parse_value(t), |t| skip_spaced_separator(t, ','))
		.collect::<Result<_, _>>()?;

	skip_whitespace(t);
	if !t.token(')') {
		return Err(Some(ParseComplexError::ExpectedCloserToMatch(')', start).at_one(t.offset())));
	}
	Ok(Composite::Unnamed(vals))
}

// Parse a variant like `Variant { hello: "there" }` or `Foo (123, true)`
fn parse_variant(t: &mut impl Tokens<Item = char>) -> Result<Variant<()>, Option<ParseError>> {
	let ident = match parse_optional_variant_ident(t) {
		Some(ident) => ident,
		None => return Err(None),
	};

	skip_whitespace(t);

	let composite = yap::one_of!(t;
		transpose_err(parse_named_composite(t)),
		transpose_err(parse_unnamed_composite(t))
	);

	match composite {
		Some(Ok(values)) => Ok(Variant { name: ident, values }),
		Some(Err(e)) => Err(Some(e)),
		None => Err(None),
	}
}

// Parse a sequence of bits like `<01101>` or `<>` into a bit sequence.
fn parse_bit_sequence(t: &mut impl Tokens<Item = char>) -> Result<BitSequence, Option<ParseError>> {
	let start = t.offset();
	if !t.token('<') {
		return Err(None);
	}
	let bits = t.tokens_while(|&c| c == '0' || c == '1').map(|c| c == '1');
	let mut seq = BitSequence::new();
	for bit in bits {
		seq.push(bit);
	}
	if !t.token('>') {
		return Err(Some(
			ParseBitSequenceError::ExpectedClosingBracketToMatch(start)
				.between(t.offset(), t.offset() + 1),
		));
	}
	Ok(seq)
}

// Parse a bool (`true` or `false`)
fn parse_bool(t: &mut impl Tokens<Item = char>) -> Option<bool> {
	let bool = if t.tokens("true".chars()) {
		Some(true)
	} else if t.tokens("false".chars()) {
		Some(false)
	} else {
		None
	};

	bool
}

// Parse a char like `'a'`
fn parse_char(t: &mut impl Tokens<Item = char>) -> Result<char, Option<ParseError>> {
	let start = t.offset();
	if !t.token('\'') {
		return Err(None);
	}
	let char = match t.next() {
		None => return Err(Some(ParseCharError::ExpectedValidCharacter.at_one(t.offset()))),
		Some(c) => c,
	};

	// If char is a backslash, it's an escape code and we
	// need to unescape it to find our inner char:
	let char = if char == '\\' {
		let escape_code = match t.next() {
			None => return Err(Some(ParseCharError::ExpectedValidEscapeCode.at_one(t.offset()))),
			Some(c) => c,
		};
		match string_helpers::from_escape_code(escape_code) {
			None => return Err(Some(ParseCharError::ExpectedValidEscapeCode.at_one(t.offset()))),
			Some(c) => c,
		}
	} else {
		char
	};

	if !t.token('\'') {
		return Err(Some(ParseCharError::ExpectedClosingQuoteToMatch(start).at_one(t.offset())));
	}
	Ok(char)
}

// Parse a number like `-123_456` or `234` or `+1234_5`
fn parse_number(t: &mut impl Tokens<Item = char>) -> Result<Primitive, Option<ParseError>> {
	let start_loc = t.offset();
	let is_positive = t.token('+') || !t.token('-');

	// When we iterate numeric digits, prefix a sign as needed:
	let sign = if is_positive { "".chars() } else { "-".chars() };

	// Now, we expect a digit and then a mix of digits and underscores:
	let mut seen_n = false;
	let digits = t
		.tokens_while(|c| {
			if c.is_digit(10) {
				seen_n = true;
				true
			} else {
				seen_n && *c == '_'
			}
		})
		.filter(|c| c.is_digit(10));

	// Chain sign to digits and attempt to parse into a number.
	let n_str: String = sign.chain(digits).collect();
	let end_loc = t.offset();

	// Nothing was parsed; Return None.
	if end_loc == start_loc {
		return Err(None);
	}

	// No digits were parsed but a sign was; err.
	if !seen_n {
		return Err(Some(ParseNumberError::ExpectedDigit.between(end_loc, end_loc + 1)));
	}

	// Parse into a number as best we can:
	if is_positive {
		n_str
			.parse::<u128>()
			.map(Primitive::uint)
			.map_err(|e| Some(ParseNumberError::ParsingFailed(e).between(start_loc, end_loc)))
	} else {
		n_str
			.parse::<i128>()
			.map(Primitive::int)
			.map_err(|e| Some(ParseNumberError::ParsingFailed(e).between(start_loc, end_loc)))
	}
}

// Parse a string like `"hello\n there"`
fn parse_string(t: &mut impl Tokens<Item = char>) -> Result<String, Option<ParseError>> {
	let start = t.offset();
	if !t.token('"') {
		return Err(None);
	}

	let mut out: String = String::new();
	let mut next_is_escaped = false;
	loop {
		let pos = t.offset();
		let char = match t.next() {
			Some(c) => c,
			None => {
				return Err(Some(
					ParseStringError::ExpectedClosingQuoteToMatch(start).at_one(t.offset()),
				))
			}
		};

		match char {
			// Escape a char:
			'\\' if !next_is_escaped => {
				next_is_escaped = true;
			}
			// Handle escaped chars:
			c if next_is_escaped => match string_helpers::from_escape_code(c) {
				Some(c) => {
					out.push(c);
					next_is_escaped = false;
				}
				None => {
					return Err(Some(
						ParseStringError::ExpectedValidEscapeCode.between(pos, pos + 1),
					))
				}
			},
			// String has closed
			'"' => {
				break; // closing quote seen; done!
			}
			// All other chars pushed as-is.
			c => {
				out.push(c);
			}
		}
	}

	Ok(out)
}

// Parse a field in a named composite like `foo: 123` or `"hello there": 123`
fn parse_field_name_and_value(
	t: &mut impl Tokens<Item = char>,
) -> Result<(String, Value<()>), ParseError> {
	let name = parse_field_name(t)?;
	if !skip_spaced_separator(t, ':') {
		return Err(ParseComplexError::MissingFieldSeparator(':').at_one(t.offset()));
	}
	let value = parse_value(t)?;
	Ok((name, value))
}

// Parse a field name in a named composite like `foo` or `"hello there"`
fn parse_field_name(t: &mut impl Tokens<Item = char>) -> Result<String, ParseError> {
	let field_name = yap::one_of!(t;
		transpose_err(parse_string(t)),
		Some(parse_ident(t)),
	);

	match field_name {
		Some(Ok(name)) => Ok(name),
		Some(Err(e)) => Err(e),
		None => Err(ParseComplexError::InvalidFieldName.at(t.offset())),
	}
}

// Parse an ident used for the variant name, like `MyVariant` or the special case
// `i"My variant name"` for idents that are not normally valid variant names, but
// can be set in Value variants (this is to ensure that we can display and then parse
// as many user-generated Values as possible).
fn parse_optional_variant_ident(t: &mut impl Tokens<Item = char>) -> Option<String> {
	fn parse_i_string(t: &mut impl Tokens<Item = char>) -> Option<String> {
		if t.next()? != 'v' {
			return None;
		}
		parse_string(t).ok()
	}

	yap::one_of!(t;
		parse_i_string(t),
		parse_ident(t).ok()
	)
}

// Parse an ident like `foo` or `MyVariant`
fn parse_ident(t: &mut impl Tokens<Item = char>) -> Result<String, ParseError> {
	let start = t.location();
	if t.skip_tokens_while(|c| c.is_alphabetic()) == 0 {
		return Err(ParseComplexError::InvalidStartingCharacterInIdent.at_one(start.offset()));
	}
	t.skip_tokens_while(|c| c.is_alphanumeric() || *c == '_');
	let end = t.location();

	let ident_str = t.slice(start, end).collect();
	Ok(ident_str)
}

// Skip any whitespace characters
fn skip_whitespace(t: &mut impl Tokens<Item = char>) {
	t.skip_tokens_while(|c| c.is_whitespace());
}

// Skip a provided separator, with optional spaces on either side
fn skip_spaced_separator(t: &mut impl Tokens<Item = char>, s: char) -> bool {
	skip_whitespace(t);
	let is_sep = t.token(s);
	skip_whitespace(t);
	is_sep
}

// Turn a ` Result<T, Option<E>>` into `Option<Result<T, E>>`.
fn transpose_err<T, E>(r: Result<T, Option<E>>) -> Option<Result<T, E>> {
	match r {
		Ok(val) => Some(Ok(val)),
		Err(Some(e)) => Some(Err(e)),
		Err(None) => None,
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn from(s: &str) -> Result<Value<()>, ParseError> {
		let (res, remaining) = from_str(s);
		if res.is_ok() {
			// all successful parse tests fully consume the input string:
			assert_eq!(remaining.len(), 0, "was not expecting any unparsed output");
		}
		res
	}

	#[test]
	fn parse_bools() {
		assert_eq!(from("true"), Ok(Value::bool(true)));
		assert_eq!(from("false"), Ok(Value::bool(false)));
	}

	#[test]
	fn parse_numbers() {
		assert_eq!(from("123"), Ok(Value::uint(123u128)));
		assert_eq!(from("1_234_56"), Ok(Value::uint(123_456_u128)));
		assert_eq!(from("+1_234_56"), Ok(Value::uint(123_456_u128)));
		assert_eq!(from("-123_4"), Ok(Value::int(-1234)));
		assert_eq!(from("-abc"), Err(ParseNumberError::ExpectedDigit.between(1, 2)));
	}

	#[test]
	fn parse_chars() {
		assert_eq!(from("'a'"), Ok(Value::char('a')));
		assert_eq!(from("'😀'"), Ok(Value::char('😀')));
		assert_eq!(from("'\\n'"), Ok(Value::char('\n')));
		assert_eq!(from("'\\t'"), Ok(Value::char('\t')));
		assert_eq!(from("'\\\"'"), Ok(Value::char('"')));
		assert_eq!(from("'\\\''"), Ok(Value::char('\'')));
		assert_eq!(from("'\\r'"), Ok(Value::char('\r')));
		assert_eq!(from("'\\\\'"), Ok(Value::char('\\')));
		assert_eq!(from("'\\0'"), Ok(Value::char('\0')));
		assert_eq!(from("'a"), Err(ParseCharError::ExpectedClosingQuoteToMatch(0).at_one(2)));
	}

	#[test]
	fn parse_strings() {
		assert_eq!(from("\"\\n \\r \\t \\0 \\\"\""), Ok(Value::string("\n \r \t \0 \"")));
		assert_eq!(from("\"Hello there 😀\""), Ok(Value::string("Hello there 😀")));
		assert_eq!(from("\"Hello\\n\\t there\""), Ok(Value::string("Hello\n\t there")));
		assert_eq!(from("\"Hello\\\\ there\""), Ok(Value::string("Hello\\ there")));
		assert_eq!(
			from("\"Hello\\p there\""),
			Err(ParseStringError::ExpectedValidEscapeCode.between(7, 8))
		);
		assert_eq!(from("\"Hi"), Err(ParseStringError::ExpectedClosingQuoteToMatch(0).at_one(3)));
	}

	#[test]
	fn parse_unnamed_composites() {
		assert_eq!(
			from("(  true, 1234 ,\t\n\t \"Hello!\" )"),
			Ok(Value::unnamed_composite(vec![
				Value::bool(true),
				Value::uint(1234u128),
				Value::string("Hello!")
			]))
		);
		assert_eq!(from("()"), Ok(Value::unnamed_composite(vec![])));
		assert_eq!(from("(\n\n\t\t\n)"), Ok(Value::unnamed_composite(vec![])));
	}

	#[test]
	fn parse_named_composites() {
		assert_eq!(
			from(
				"{
            hello: true,
            foo: 1234,
            \"Hello there 😀\": \"Hello!\"
        }"
			),
			Ok(Value::named_composite(vec![
				("hello".into(), Value::bool(true)),
				("foo".into(), Value::uint(1234u128)),
				("Hello there 😀".into(), Value::string("Hello!"))
			]))
		);
	}

	#[test]
	fn parse_variants() {
		assert_eq!(
			from(
				"MyVariant {
            hello: true,
            foo: 1234,
            \"Hello there 😀\": \"Hello!\"
        }"
			),
			Ok(Value::named_variant(
				"MyVariant",
				vec![
					("hello".into(), Value::bool(true)),
					("foo".into(), Value::uint(1234u128)),
					("Hello there 😀".into(), Value::string("Hello!"))
				]
			))
		);

		assert_eq!(
			from("Foo (  true, 1234 ,\t\n\t \"Hello!\" )"),
			Ok(Value::unnamed_variant(
				"Foo",
				vec![Value::bool(true), Value::uint(1234u128), Value::string("Hello!")]
			))
		);

		assert_eq!(from("Foo()"), Ok(Value::unnamed_variant("Foo", vec![])));
		assert_eq!(from("Foo{}"), Ok(Value::named_variant("Foo", vec![])));
		assert_eq!(from("Foo( \t)"), Ok(Value::unnamed_variant("Foo", vec![])));
		assert_eq!(from("Foo{  }"), Ok(Value::named_variant("Foo", vec![])));

		// Parsing special "v" strings:
		assert_eq!(
			from("v\"variant name\" {  }"),
			Ok(Value::named_variant("variant name", vec![]))
		);
	}

	#[test]
	fn parse_bit_sequences() {
		use bitvec::{bitvec, order::Lsb0};
		assert_eq!(
			from("<011010110101101>"),
			Ok(Value::bit_sequence(bitvec![u8, Lsb0; 0,1,1,0,1,0,1,1,0,1,0,1,1,0,1]))
		);
		assert_eq!(from("<01101>"), Ok(Value::bit_sequence(bitvec![u8, Lsb0; 0,1,1,0,1])));
		assert_eq!(from("<0>"), Ok(Value::bit_sequence(bitvec![u8, Lsb0; 0])));
		assert_eq!(from("<>"), Ok(Value::bit_sequence(bitvec![u8, Lsb0;])));
	}
}
