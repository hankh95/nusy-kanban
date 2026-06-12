//! Typed-literal parsing and comparison (EX-4681, VY-4679 E2).
//!
//! The triples store keeps the `object` column as Utf8 — the **lexical form is
//! authoritative and never destroyed**. The `object_datatype` sidecar column
//! ([`crate::schema::col::OBJECT_DATATYPE`]) carries an XSD datatype URI so that a
//! rule/condition evaluator can compare values *by their type* rather than
//! lexicographically: `"9" < "10"` numerically (not as strings), `140 > 130` as
//! decimals, and dates in chronological order.
//!
//! This module is the parse-and-compare primitive EX-4690 (guideline threshold
//! conditions) consumes. It does not mutate the store; it reads a
//! `(lexical, datatype)` pair and yields a comparable [`TypedValue`].
//!
//! Supported XSD subset: `decimal`, `integer`, `date`, `dateTime`, `boolean`,
//! `string`. Numeric types (`decimal`/`integer`) are cross-comparable. An
//! unrecognized or unparseable datatype falls back to a plain lexical string,
//! so comparison is always defined (never panics).

use std::cmp::Ordering;

use chrono::{DateTime, FixedOffset, NaiveDate};

/// XSD `decimal` datatype URI.
pub const XSD_DECIMAL: &str = "http://www.w3.org/2001/XMLSchema#decimal";
/// XSD `integer` datatype URI.
pub const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
/// XSD `double` datatype URI (parsed as decimal).
pub const XSD_DOUBLE: &str = "http://www.w3.org/2001/XMLSchema#double";
/// XSD `date` datatype URI.
pub const XSD_DATE: &str = "http://www.w3.org/2001/XMLSchema#date";
/// XSD `dateTime` datatype URI.
pub const XSD_DATETIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";
/// XSD `boolean` datatype URI.
pub const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";
/// XSD `string` datatype URI (explicit plain string).
pub const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// A parsed, comparable view over an `(object lexical, datatype)` pair.
///
/// This is the *comparison* value only — the canonical form remains the lexical
/// string in the store's `object` column. Construct with [`parse`].
#[derive(Debug, Clone, PartialEq)]
pub enum TypedValue {
    /// `xsd:decimal` / `xsd:integer` / `xsd:double` — all numeric, cross-comparable.
    Number(f64),
    /// `xsd:date` (no time component).
    Date(NaiveDate),
    /// `xsd:dateTime` (timezone-aware).
    DateTime(DateTime<FixedOffset>),
    /// `xsd:boolean`.
    Boolean(bool),
    /// Plain string: no datatype, `xsd:string`, or an unparseable typed literal.
    /// Compared lexically.
    Plain(String),
}

/// Parse a `(lexical, datatype)` pair into a comparable [`TypedValue`].
///
/// `datatype` is the XSD URI (or `None` for a plain string). A typed literal
/// whose lexical form does not parse for its datatype degrades to
/// [`TypedValue::Plain`] rather than failing — comparison stays total.
pub fn parse(lexical: &str, datatype: Option<&str>) -> TypedValue {
    match datatype {
        None | Some(XSD_STRING) => TypedValue::Plain(lexical.to_string()),
        Some(XSD_INTEGER) | Some(XSD_DECIMAL) | Some(XSD_DOUBLE) => lexical
            .trim()
            .parse::<f64>()
            .map(TypedValue::Number)
            .unwrap_or_else(|_| TypedValue::Plain(lexical.to_string())),
        Some(XSD_DATE) => NaiveDate::parse_from_str(lexical.trim(), "%Y-%m-%d")
            .map(TypedValue::Date)
            .unwrap_or_else(|_| TypedValue::Plain(lexical.to_string())),
        Some(XSD_DATETIME) => DateTime::parse_from_rfc3339(lexical.trim())
            .map(TypedValue::DateTime)
            .unwrap_or_else(|_| TypedValue::Plain(lexical.to_string())),
        Some(XSD_BOOLEAN) => match lexical.trim() {
            "true" | "1" => TypedValue::Boolean(true),
            "false" | "0" => TypedValue::Boolean(false),
            _ => TypedValue::Plain(lexical.to_string()),
        },
        // Unknown datatype URI — keep the lexical form, compare as a string.
        Some(_) => TypedValue::Plain(lexical.to_string()),
    }
}

impl TypedValue {
    /// Order two typed values. Returns `None` when the two are *incomparable*
    /// (e.g. a number vs a date, or a boolean vs anything else) — the caller
    /// (rule evaluator) treats `None` as "condition does not apply".
    ///
    /// `NaN` numbers are incomparable (`None`), matching IEEE semantics.
    pub fn partial_cmp_typed(&self, other: &TypedValue) -> Option<Ordering> {
        use TypedValue::*;
        match (self, other) {
            (Number(a), Number(b)) => a.partial_cmp(b),
            (Date(a), Date(b)) => Some(a.cmp(b)),
            (DateTime(a), DateTime(b)) => Some(a.cmp(b)),
            (Boolean(a), Boolean(b)) => Some(a.cmp(b)),
            (Plain(a), Plain(b)) => Some(a.cmp(b)),
            // Cross-type comparisons are undefined.
            _ => None,
        }
    }
}

/// Compare two object literals directly from their `(lexical, datatype)` pairs —
/// the shape a condition evaluator (EX-4690) uses to test a threshold like
/// `subject.bp > 130`. Returns `None` if the two values are incomparable.
pub fn compare_objects(
    a_lexical: &str,
    a_datatype: Option<&str>,
    b_lexical: &str,
    b_datatype: Option<&str>,
) -> Option<Ordering> {
    parse(a_lexical, a_datatype).partial_cmp_typed(&parse(b_lexical, b_datatype))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_compares_numerically_not_lexically() {
        // 140 > 130 as decimals.
        assert_eq!(
            compare_objects("140", Some(XSD_DECIMAL), "130", Some(XSD_DECIMAL)),
            Some(Ordering::Greater)
        );
        // "9" < "10" numerically — the bug lexicographic comparison would get wrong.
        assert_eq!(
            compare_objects("9", Some(XSD_INTEGER), "10", Some(XSD_INTEGER)),
            Some(Ordering::Less)
        );
        // Sanity: lexicographically "9" > "10", so this proves typing matters.
        assert_eq!("9".cmp("10"), Ordering::Greater);
    }

    #[test]
    fn integer_and_decimal_cross_compare() {
        assert_eq!(
            compare_objects("140", Some(XSD_INTEGER), "139.5", Some(XSD_DECIMAL)),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn dates_order_chronologically() {
        assert_eq!(
            compare_objects("2014-01-01", Some(XSD_DATE), "2014-12-31", Some(XSD_DATE)),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_objects("2020-06-15", Some(XSD_DATE), "2020-06-15", Some(XSD_DATE)),
            Some(Ordering::Equal)
        );
    }

    #[test]
    fn datetimes_order_with_timezone() {
        // Same instant, different offsets → equal.
        assert_eq!(
            compare_objects(
                "2020-01-01T12:00:00Z",
                Some(XSD_DATETIME),
                "2020-01-01T13:00:00+01:00",
                Some(XSD_DATETIME),
            ),
            Some(Ordering::Equal)
        );
    }

    #[test]
    fn untyped_falls_back_to_lexical() {
        assert_eq!(
            compare_objects("apple", None, "banana", None),
            Some(Ordering::Less)
        );
        // Plain "9" vs "10" with no datatype is lexical (string) ordering.
        assert_eq!(
            compare_objects("9", None, "10", None),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn cross_type_is_incomparable() {
        assert_eq!(
            compare_objects("140", Some(XSD_DECIMAL), "2020-01-01", Some(XSD_DATE)),
            None
        );
        assert_eq!(
            compare_objects("true", Some(XSD_BOOLEAN), "1", Some(XSD_INTEGER)),
            None
        );
    }

    #[test]
    fn unparseable_typed_literal_degrades_to_plain() {
        // A non-numeric lexical tagged decimal degrades to Plain (no panic),
        // and then compares lexically against another Plain.
        assert_eq!(
            parse("not-a-number", Some(XSD_DECIMAL)),
            TypedValue::Plain("not-a-number".to_string())
        );
        assert_eq!(
            parse("xyz", Some(XSD_DATE)),
            TypedValue::Plain("xyz".to_string())
        );
    }

    #[test]
    fn unknown_datatype_uri_is_plain() {
        assert_eq!(
            parse("v", Some("http://example.org/custom")),
            TypedValue::Plain("v".to_string())
        );
    }

    #[test]
    fn booleans_compare() {
        assert_eq!(
            compare_objects("false", Some(XSD_BOOLEAN), "true", Some(XSD_BOOLEAN)),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn nan_is_incomparable() {
        assert_eq!(
            parse("NaN", Some(XSD_DECIMAL)).partial_cmp_typed(&parse("1", Some(XSD_DECIMAL))),
            None
        );
    }
}
