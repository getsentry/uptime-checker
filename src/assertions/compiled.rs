use http::HeaderValue;
use jsonpath_rust::{
    parser::{errors::JsonPathError, model::JpQuery, parse_json_path},
    query::js_path_process,
};
use std::{
    borrow::{Borrow, BorrowMut},
    str::FromStr,
};

const GLOB_COMPLEXITY_LIMIT: u64 = 20;
const ASSERTION_MAX_GAS: u32 = 100;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Invalid glob: {0}")]
    InvalidGlob(String),

    #[error("JSONPath Parser Error: {msg}")]
    JSONPathParser {
        msg: String,
        path: String,
        pos: usize,
    },

    #[error("Invalid JSONPath: {msg}")]
    InvalidJsonPath { msg: String },

    #[error("Assertion took too long")]
    TookTooLong,

    #[error("Invalid Body JSON: {0}")]
    InvalidBodyJson(String),
}

struct Gas(u32);

impl Gas {
    fn consume(&mut self, amount: u32) -> Result<(), Error> {
        if amount > self.0 {
            return Err(Error::TookTooLong);
        }
        self.0 -= amount;

        Ok(())
    }
}

impl Borrow<u32> for Gas {
    fn borrow(&self) -> &u32 {
        &self.0
    }
}

impl BorrowMut<u32> for Gas {
    fn borrow_mut(&mut self) -> &mut u32 {
        &mut self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    root: Op,
}

impl Assertion {
    pub fn eval(
        &self,
        status_code: u16,
        headers: &hyper::header::HeaderMap<HeaderValue>,
        body: &[u8],
    ) -> Result<bool, Error> {
        let mut gas = Gas(ASSERTION_MAX_GAS);
        self.root.eval(status_code, headers, body, &mut gas)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Comparison {
    LessThan,
    GreaterThan,
    Equal,
    NotEqual,
}

impl From<&super::Comparison> for Comparison {
    fn from(value: &super::Comparison) -> Self {
        match *value {
            super::Comparison::LessThan => Comparison::LessThan,
            super::Comparison::GreaterThan => Comparison::GreaterThan,
            super::Comparison::Equal => Comparison::Equal,
            super::Comparison::NotEqual => Comparison::NotEqual,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HeaderOperand {
    Literal { value: Value },
    Glob { value: relay_pattern::Pattern },
}

impl TryFrom<&super::HeaderOperand> for HeaderOperand {
    type Error = Error;
    fn try_from(value: &super::HeaderOperand) -> Result<Self, Self::Error> {
        let v = match value {
            super::HeaderOperand::Literal { value } => HeaderOperand::Literal {
                value: value.into(),
            },
            super::HeaderOperand::Glob { pattern: value } => {
                let r = relay_pattern::Pattern::builder(&value.value)
                    .max_complexity(GLOB_COMPLEXITY_LIMIT)
                    .build();

                match r {
                    Ok(value) => HeaderOperand::Glob { value },
                    Err(e) => return Err(Error::InvalidGlob(e.to_string())),
                }
            }
        };

        Ok(v)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Value {
    I64(i64),
    F64(f64),
    String(String),
}

impl Eq for Value {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HeaderComparison {
    Always,
    Never,
    Equals { test_value: HeaderOperand },
    NotEquals { test_value: HeaderOperand },
    LessThan { test_value: Value },
    GreaterThan { test_value: Value },
}

impl From<&String> for Value {
    fn from(value: &String) -> Self {
        let f = value.parse::<f64>();
        if let Ok(f) = f {
            return Value::F64(f);
        }

        let i = value.parse::<i64>();
        if let Ok(i) = i {
            return Value::I64(i);
        }

        Value::String(value.clone())
    }
}

impl TryFrom<&super::HeaderComparison> for HeaderComparison {
    type Error = Error;
    fn try_from(value: &super::HeaderComparison) -> Result<Self, Self::Error> {
        let v = match value {
            super::HeaderComparison::Always => HeaderComparison::Always,
            super::HeaderComparison::Never => HeaderComparison::Never,
            super::HeaderComparison::Equals { test_value } => HeaderComparison::Equals {
                test_value: test_value.try_into()?,
            },
            super::HeaderComparison::NotEquals { test_value } => HeaderComparison::NotEquals {
                test_value: test_value.try_into()?,
            },
            super::HeaderComparison::LessThan { test_value } => HeaderComparison::LessThan {
                test_value: test_value.into(),
            },
            super::HeaderComparison::GreaterThan { test_value } => HeaderComparison::GreaterThan {
                test_value: test_value.into(),
            },
        };

        Ok(v)
    }
}

fn cmp_eq<T>(header_value: &str, value: &T) -> bool
where
    T: FromStr + PartialEq,
{
    header_value
        .parse::<T>()
        .map(|v| v == *value)
        .unwrap_or(false)
}

fn cmp_lt<T>(header_value: &str, value: &T) -> bool
where
    T: FromStr + PartialOrd,
{
    header_value
        .parse::<T>()
        .map(|v| v < *value)
        .unwrap_or(false)
}

fn cmp_gt<T>(header_value: &str, value: &T) -> bool
where
    T: FromStr + PartialOrd,
{
    header_value
        .parse::<T>()
        .map(|v| v > *value)
        .unwrap_or(false)
}

fn cmp_eq_header(
    header_value: &str,
    test_value: &HeaderOperand,
    gas: &mut Gas,
) -> Result<bool, Error> {
    let result = match test_value {
        HeaderOperand::Literal { value } => match value {
            Value::I64(value) => cmp_eq(header_value, value),
            Value::F64(value) => cmp_eq(header_value, value),
            Value::String(value) => cmp_eq(header_value, value),
        },
        HeaderOperand::Glob { value } => {
            // TODO: either expose, or copy, the complexity metric from relay-pattern.
            gas.consume(5)?;
            value.is_match(header_value)
        }
    };

    Ok(result)
}

impl HeaderComparison {
    fn eval(&self, header_value: &str, gas: &mut Gas) -> Result<bool, Error> {
        let result = match self {
            HeaderComparison::Always => true,
            HeaderComparison::Never => false,
            HeaderComparison::Equals { test_value } => {
                cmp_eq_header(header_value, test_value, gas)?
            }
            HeaderComparison::NotEquals { test_value } => {
                !cmp_eq_header(header_value, test_value, gas)?
            }

            HeaderComparison::LessThan { test_value } => match test_value {
                Value::I64(value) => cmp_lt(header_value, value),
                Value::F64(value) => cmp_lt(header_value, value),
                Value::String(value) => cmp_lt(header_value, value),
            },
            HeaderComparison::GreaterThan { test_value } => match test_value {
                Value::I64(value) => cmp_gt(header_value, value),
                Value::F64(value) => cmp_gt(header_value, value),
                Value::String(value) => cmp_gt(header_value, value),
            },
        };

        Ok(result)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Op {
    And {
        children: Vec<Op>,
    },
    Or {
        children: Vec<Op>,
    },
    Not {
        operand: Box<Op>,
    },
    StatusCodeCheck {
        value: u16,
        operator: Comparison,
    },
    JsonPath {
        value: JpQuery,
    },
    HeaderCheck {
        key: HeaderComparison,
        value: HeaderComparison,
    },
}

impl Eq for Op {}

impl Op {
    fn eval(
        &self,
        status_code: u16,
        headers: &hyper::header::HeaderMap<HeaderValue>,
        body: &[u8],
        gas: &mut Gas,
    ) -> Result<bool, Error> {
        let result = match self {
            Op::And { children } => {
                let mut result = true;
                for child in children.iter() {
                    result &= child.eval(status_code, headers, body, gas)?;
                    if !result {
                        break;
                    }
                }
                result
            }
            Op::Or { children } => {
                let mut result = false;
                for child in children.iter() {
                    result |= child.eval(status_code, headers, body, gas)?;
                    if result {
                        break;
                    }
                }
                result
            }
            Op::Not { operand } => !operand.eval(status_code, headers, body, gas)?,
            Op::StatusCodeCheck { value, operator } => {
                gas.consume(1)?;
                match operator {
                    Comparison::LessThan => status_code < *value,
                    Comparison::GreaterThan => status_code > *value,
                    Comparison::Equal => *value == status_code,
                    Comparison::NotEqual => *value != status_code,
                }
            }
            Op::HeaderCheck { key, value } => {
                // Find any header that passes key.  Then, see if it's value passes value.
                let mut result = false;

                for (k, v) in headers {
                    if key.eval(k.as_str(), gas)? {
                        if let Ok(value_str) = v.to_str() {
                            if value.eval(value_str, gas)? {
                                result = true;
                                break;
                            }
                        }
                    }
                }

                result
            }
            Op::JsonPath { value } => {
                let json: serde_json::Value = serde_json::from_slice(body)
                    .map_err(|e| Error::InvalidBodyJson(e.to_string()))?;
                let result = js_path_process(value, &json, gas.borrow_mut())?;
                !result.is_empty()
            }
        };

        Ok(result)
    }
}

pub fn compile(assertion: &super::Assertion) -> Result<Assertion, Error> {
    Ok(Assertion {
        root: compile_op(&assertion.root)?,
    })
}

fn compile_op(op: &super::Op) -> Result<Op, Error> {
    let op = match op {
        super::Op::And { children } => Op::And {
            children: visit_children(children)?,
        },
        super::Op::Or { children } => Op::Or {
            children: visit_children(children)?,
        },
        super::Op::Not { operand } => Op::Not {
            operand: Box::new(compile_op(operand)?),
        },
        super::Op::StatusCodeCheck { value, operator } => Op::StatusCodeCheck {
            value: *value,
            operator: operator.into(),
        },
        super::Op::HeaderCheck { key, value } => Op::HeaderCheck {
            key: key.try_into()?,
            value: value.try_into()?,
        },
        super::Op::JsonPath { value } => Op::JsonPath {
            value: parse_json_path(value)?,
        },
    };

    Ok(op)
}

impl From<JsonPathError> for Error {
    fn from(value: JsonPathError) -> Self {
        match value {
            JsonPathError::PestError(error) => {
                let pos = match error.location {
                    pest::error::InputLocation::Pos(pos) => pos,
                    pest::error::InputLocation::Span((start, _)) => start,
                };
                Error::JSONPathParser {
                    msg: error.to_string(),
                    path: error.line().to_owned(),
                    pos,
                }
            }
            JsonPathError::TookTooLong => Error::TookTooLong,
            JsonPathError::InvalidGlob(s) => Error::InvalidGlob(s),
            rest => Error::InvalidJsonPath {
                msg: rest.to_string(),
            },
        }
    }
}

fn visit_children(children: &[super::Op]) -> Result<Vec<Op>, Error> {
    let mut cs = vec![];
    for c in children.iter() {
        cs.push(compile_op(c)?);
    }
    Ok(cs)
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue};

    use crate::assertions::{
        compiled::compile, Assertion, Comparison, GlobPattern, HeaderComparison, HeaderOperand, Op,
    };

    #[test]
    fn test_status_comparators_and() {
        let assert = Assertion {
            root: Op::And {
                children: vec![
                    Op::StatusCodeCheck {
                        value: 100,
                        operator: Comparison::GreaterThan,
                    },
                    Op::StatusCodeCheck {
                        value: 300,
                        operator: Comparison::LessThan,
                    },
                    Op::StatusCodeCheck {
                        value: 200,
                        operator: Comparison::Equal,
                    },
                    Op::StatusCodeCheck {
                        value: 400,
                        operator: Comparison::NotEqual,
                    },
                ],
            },
        };

        let assert = compile(&assert).unwrap();
        assert!(assert.eval(200, &HeaderMap::new(), b"").unwrap());
        assert!(!assert.eval(105, &HeaderMap::new(), b"").unwrap());
        assert!(!assert.eval(95, &HeaderMap::new(), b"").unwrap());
        assert!(!assert.eval(299, &HeaderMap::new(), b"").unwrap());
        assert!(!assert.eval(305, &HeaderMap::new(), b"").unwrap());
    }

    #[test]
    fn test_status_comparators_or() {
        let assert = Assertion {
            root: Op::Or {
                children: vec![
                    Op::StatusCodeCheck {
                        value: 100,
                        operator: Comparison::LessThan,
                    },
                    Op::StatusCodeCheck {
                        value: 300,
                        operator: Comparison::GreaterThan,
                    },
                    Op::StatusCodeCheck {
                        value: 200,
                        operator: Comparison::Equal,
                    },
                ],
            },
        };
        let assert = compile(&assert).unwrap();
        assert!(assert.eval(200, &HeaderMap::new(), b"").unwrap());
        assert!(!assert.eval(105, &HeaderMap::new(), b"").unwrap());
        assert!(assert.eval(95, &HeaderMap::new(), b"").unwrap());
        assert!(!assert.eval(299, &HeaderMap::new(), b"").unwrap());
        assert!(assert.eval(305, &HeaderMap::new(), b"").unwrap());
    }

    #[test]
    fn test_booleans_nesting() {
        let assert = Assertion {
            root: Op::Or {
                children: vec![
                    Op::And {
                        children: vec![
                            Op::StatusCodeCheck {
                                value: 0,
                                operator: Comparison::GreaterThan,
                            },
                            Op::StatusCodeCheck {
                                value: 100,
                                operator: Comparison::LessThan,
                            },
                        ],
                    },
                    Op::Not {
                        operand: Op::And {
                            children: vec![Op::StatusCodeCheck {
                                value: 200,
                                operator: Comparison::NotEqual,
                            }],
                        }
                        .into(),
                    },
                    Op::Or {
                        children: vec![
                            Op::StatusCodeCheck {
                                value: 201,
                                operator: Comparison::Equal,
                            },
                            Op::StatusCodeCheck {
                                value: 202,
                                operator: Comparison::Equal,
                            },
                        ],
                    },
                ],
            },
        };

        let assert = compile(&assert).unwrap();
        assert!(!assert.eval(0, &HeaderMap::new(), b"").unwrap());
        assert!(assert.eval(15, &HeaderMap::new(), b"").unwrap());
        assert!(!assert.eval(100, &HeaderMap::new(), b"").unwrap());
        assert!(assert.eval(200, &HeaderMap::new(), b"").unwrap());
        assert!(assert.eval(201, &HeaderMap::new(), b"").unwrap());
        assert!(assert.eval(202, &HeaderMap::new(), b"").unwrap());
        assert!(!assert.eval(203, &HeaderMap::new(), b"").unwrap());
    }

    #[test]
    fn test_gas_exhaustion() {
        let mut hmap = HeaderMap::new();
        hmap.append("x-header-good", HeaderValue::from_static("0"));
        hmap.append("x-header-good-a", HeaderValue::from_static("1"));
        hmap.append("x-header-good-1", HeaderValue::from_static("2"));
        hmap.append("x-header-good-2", HeaderValue::from_static("0"));
        hmap.append("x-header-good-b", HeaderValue::from_static("1"));
        hmap.append("x-header-good-3", HeaderValue::from_static("2"));

        let body = r#"
  [
    {
      "prop1": "aaaaa"
    },
    {
      "prop1": "aa"
    },
    {
      "prop1": "aaaa"
    },
    {
      "prop1": "aaaa"
    },
    {
      "prop1": "aa"
    },
    {
      "prop1": "aaaa"
    },
    {
      "prop1": "aaaaa"
    }
  ]
"#;

        let jsonpath = Op::JsonPath {
            value: "$[?length(@.prop1) > 4]".to_owned(),
        };
        let headercheck = Op::HeaderCheck {
            key: HeaderComparison::Equals {
                test_value: HeaderOperand::Glob {
                    pattern: GlobPattern {
                        value: "*-good*".to_owned(),
                    },
                },
            },
            value: HeaderComparison::Always,
        };

        let assert = Assertion {
            root: Op::And {
                children: vec![
                    jsonpath.clone(),
                    jsonpath.clone(),
                    jsonpath.clone(),
                    jsonpath.clone(),
                    jsonpath.clone(),
                    headercheck.clone(),
                    headercheck.clone(),
                    headercheck.clone(),
                    headercheck.clone(),
                    headercheck.clone(),
                    headercheck.clone(),
                ],
            },
        };

        let assert = compile(&assert).unwrap();

        let result = assert.eval(200, &hmap, body.as_bytes()).err().unwrap();
        assert!(matches!(result, super::Error::TookTooLong));
    }

    #[test]
    fn test_json_path() {
        let body = r#"
  [
    {
      "prop1": "a"
    },
    {
      "prop1": "aa"
    },
    {
      "prop1": "aaaa"
    },
    {
      "prop1": "aaaaa"
    }
  ]
"#;
        let hmap = HeaderMap::new();
        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?length(@.prop1) > 4]".to_owned(),
            },
        };

        let assert = compile(&assert).unwrap();

        assert!(assert.eval(200, &hmap, body.as_bytes()).unwrap());

        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?length(@.prop1) > 10]".to_owned(),
            },
        };

        let assert = compile(&assert).unwrap();

        assert!(!assert.eval(200, &hmap, body.as_bytes()).unwrap());

        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?glob(@.prop1, \"a[a-z]*\")]".to_owned(),
            },
        };

        let assert = compile(&assert).unwrap();
        assert!(assert.eval(200, &hmap, body.as_bytes()).unwrap());

        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?glob(@.prop1, \"A[A-Z]*\")]".to_owned(),
            },
        };

        let assert = compile(&assert).unwrap();

        assert!(!assert.eval(200, &hmap, body.as_bytes()).unwrap());
    }

    #[test]
    fn test_header_regex() {
        let mut hmap = HeaderMap::new();
        hmap.append("x-header-good", HeaderValue::from_static("0"));
        hmap.append("x-header-good-a", HeaderValue::from_static("1"));
        hmap.append("x-header-good-1", HeaderValue::from_static("2"));

        let assert = Assertion {
            root: Op::HeaderCheck {
                key: HeaderComparison::Equals {
                    test_value: HeaderOperand::Glob {
                        pattern: GlobPattern {
                            value: r"*-good-[0-9]".into(),
                        },
                    },
                },
                value: HeaderComparison::Always,
            },
        };
        let assert = compile(&assert).unwrap();
        assert!(assert.eval(200, &hmap, b"").unwrap());

        let assert = Assertion {
            root: Op::Or {
                children: vec![Op::HeaderCheck {
                    key: HeaderComparison::Equals {
                        test_value: HeaderOperand::Glob {
                            pattern: GlobPattern {
                                value: r"*-good-[0-9]".into(),
                            },
                        },
                    },
                    value: HeaderComparison::Equals {
                        test_value: HeaderOperand::Literal { value: "2".into() },
                    },
                }],
            },
        };
        let assert = compile(&assert).unwrap();
        assert!(assert.eval(200, &hmap, b"").unwrap());

        let assert = Assertion {
            root: Op::HeaderCheck {
                key: HeaderComparison::Equals {
                    test_value: HeaderOperand::Glob {
                        pattern: GlobPattern {
                            value: r"*-good-[0-9]".into(),
                        },
                    },
                },
                value: HeaderComparison::Equals {
                    test_value: HeaderOperand::Glob {
                        pattern: GlobPattern {
                            value: r"[0-9][0-9]".into(),
                        },
                    },
                },
            },
        };
        let assert = compile(&assert).unwrap();
        assert!(!assert.eval(200, &hmap, b"").unwrap());
    }
}
