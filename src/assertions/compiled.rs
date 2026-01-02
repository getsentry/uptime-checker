use http::HeaderValue;
use jsonpath_rust::{
    parser::{errors::JsonPathError, model::JpQuery, parse_json_path},
    query::js_path_process,
};
use serde::{Deserialize, Serialize};
use std::{
    borrow::{Borrow, BorrowMut},
    str::FromStr,
};

const GLOB_COMPLEXITY_LIMIT: u64 = 20;
const ASSERTION_MAX_GAS: u32 = 100;

#[derive(thiserror::Error, Debug, Serialize)]
pub enum RuntimeError {
    #[error("Invalid JSONPath: {msg}")]
    InvalidJsonPath { msg: String },

    #[error("Assertion took too long")]
    TookTooLong,

    #[error("Invalid Body JSON: {body}")]
    InvalidBodyJson { body: String },
}

#[derive(thiserror::Error, Debug, Serialize)]
pub enum CompilationError {
    #[error("Invalid glob {glob}: {msg}")]
    InvalidGlob { glob: String, msg: String },

    #[error("JSONPath Parser Error: {msg}")]
    JSONPathParser {
        msg: String,
        path: String,
        pos: usize,
    },

    #[error("Invalid JSONPath: {msg}")]
    InvalidJsonPath { msg: String },
}

// This struct records the reason behind the result of an assertion evaluation.  Eventually,
// an assert evaluates to 'true' or 'false', but we won't know whether we're failing until
// we're done evaluating.  To keep track of errors, we therefore keep track of the result of
// given subtree of an assertion--in the case of nodes that have more than one child, we pick
// the path that resulted in our decision, which could be all children (in the case of a
// passing AND, or a failing OR), or a single child (in the case of a failing AND or passing
// OR).  It can also be a leaf value.
pub struct EvalResult {
    pub reason_path: EvalPath,
    pub result: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvalPath {
    Leaf,
    AllChildren,
    ChildIndex { index: usize, child: Box<EvalPath> },
}

impl EvalResult {
    pub fn and_node() -> EvalResult {
        Self {
            reason_path: EvalPath::AllChildren,
            result: true,
        }
    }

    pub fn or_node() -> EvalResult {
        Self {
            reason_path: EvalPath::AllChildren,
            result: false,
        }
    }

    // Construct a leaf node that is directly responsible for the result.
    pub fn leaf(value: bool) -> EvalResult {
        Self {
            reason_path: EvalPath::Leaf,
            result: value,
        }
    }

    // Construct a node in which a single child node is responsible for
    // the result.
    pub fn child_idx(index: usize, value: bool, path: EvalPath) -> EvalResult {
        Self {
            reason_path: EvalPath::ChildIndex {
                index,
                child: path.into(),
            },
            result: value,
        }
    }
}

pub fn extract_failure_data(
    path: &EvalPath,
    assert_op: &crate::assertions::Op,
) -> crate::assertions::Op {
    match path {
        EvalPath::Leaf => assert_op.clone(),
        EvalPath::AllChildren => assert_op.clone(),
        EvalPath::ChildIndex { index, child } => match assert_op {
            crate::assertions::Op::And { children } => crate::assertions::Op::And {
                children: vec![extract_failure_data(&*child, &children[*index])],
            },
            crate::assertions::Op::Or { children } => crate::assertions::Op::Or {
                children: vec![extract_failure_data(&*child, &children[*index])],
            },
            crate::assertions::Op::Not { operand } => crate::assertions::Op::Not {
                operand: extract_failure_data(*&child, &**operand).into(),
            },
            _ => panic!(),
        },
    }
}

struct Gas(u32);

impl Gas {
    fn consume(&mut self, amount: u32) -> Result<(), RuntimeError> {
        if amount > self.0 {
            return Err(RuntimeError::TookTooLong);
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

#[derive(Debug, PartialEq, Eq)]
pub struct Assertion {
    root: Op,
}

impl Assertion {
    pub fn eval(
        &self,
        status_code: u16,
        headers: &hyper::header::HeaderMap<HeaderValue>,
        body: &[u8],
    ) -> Result<EvalResult, RuntimeError> {
        let mut gas = Gas(ASSERTION_MAX_GAS);
        self.root.eval(status_code, headers, body, &mut gas)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Comparison {
    Always,
    Never,
    LessThan,
    GreaterThan,
    Equal,
    NotEqual,
}

impl From<&super::Comparison> for Comparison {
    fn from(value: &super::Comparison) -> Self {
        match *value {
            super::Comparison::Always => Comparison::Always,
            super::Comparison::Never => Comparison::Never,
            super::Comparison::LessThan => Comparison::LessThan,
            super::Comparison::GreaterThan => Comparison::GreaterThan,
            super::Comparison::Equals => Comparison::Equal,
            super::Comparison::NotEqual => Comparison::NotEqual,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum HeaderOperand {
    None,
    Literal { value: Value },
    Glob { value: relay_pattern::Pattern },
}

impl HeaderOperand {
    pub fn eval(
        &self,
        op: &Comparison,
        header_value: &str,
        gas: &mut Gas,
    ) -> Result<bool, RuntimeError> {
        let result = match op {
            Comparison::Always => true,
            Comparison::Never => false,
            Comparison::LessThan => match self {
                HeaderOperand::None => false,
                HeaderOperand::Literal { value } => match value {
                    Value::I64(value) => cmp_lt(header_value, value),
                    Value::F64(value) => cmp_lt(header_value, value),
                    Value::String(value) => cmp_lt(header_value, value),
                },
                HeaderOperand::Glob { .. } => false,
            },
            Comparison::GreaterThan => match self {
                HeaderOperand::None => false,
                HeaderOperand::Literal { value } => match value {
                    Value::I64(value) => cmp_gt(header_value, value),
                    Value::F64(value) => cmp_gt(header_value, value),
                    Value::String(value) => cmp_gt(header_value, value),
                },
                HeaderOperand::Glob { .. } => false,
            },
            Comparison::Equal => cmp_eq_header(header_value, self, gas)?,
            Comparison::NotEqual => !cmp_eq_header(header_value, self, gas)?,
        };

        Ok(result)
    }
}

impl TryFrom<&super::HeaderOperand> for HeaderOperand {
    type Error = CompilationError;
    fn try_from(value: &super::HeaderOperand) -> Result<Self, Self::Error> {
        let v = match value {
            super::HeaderOperand::None => HeaderOperand::None,
            super::HeaderOperand::Literal { value } => HeaderOperand::Literal {
                value: value.into(),
            },
            super::HeaderOperand::Glob { pattern: value } => {
                let r = relay_pattern::Pattern::builder(&value.value)
                    .max_complexity(GLOB_COMPLEXITY_LIMIT)
                    .build();

                match r {
                    Ok(value) => HeaderOperand::Glob { value },
                    Err(e) => {
                        return Err(CompilationError::InvalidGlob {
                            glob: e.to_string(),
                            msg: e.to_string(),
                        })
                    }
                }
            }
        };

        Ok(v)
    }
}

#[derive(Debug, PartialEq)]
enum Value {
    I64(i64),
    F64(f64),
    String(String),
}

impl Eq for Value {}

#[derive(Debug, PartialEq, Eq)]
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
) -> Result<bool, RuntimeError> {
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
        HeaderOperand::None => false,
    };

    Ok(result)
}

impl HeaderComparison {
    fn eval(&self, header_value: &str, gas: &mut Gas) -> Result<bool, RuntimeError> {
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

#[derive(Debug, PartialEq)]
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
        key_op: Comparison,
        key_operand: HeaderOperand,
        value_op: Comparison,
        value_operand: HeaderOperand,
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
    ) -> Result<EvalResult, RuntimeError> {
        let result = match self {
            Op::And { children } => {
                let mut result = EvalResult::and_node();
                for (idx, child) in children.iter().enumerate() {
                    let eval = child.eval(status_code, headers, body, gas)?;
                    if !eval.result {
                        result = EvalResult::child_idx(idx, eval.result, eval.reason_path);
                        break;
                    }
                }
                result
            }
            Op::Or { children } => {
                let mut result = EvalResult::or_node();
                for (idx, child) in children.iter().enumerate() {
                    let eval = child.eval(status_code, headers, body, gas)?;
                    if eval.result {
                        result = EvalResult::child_idx(idx, eval.result, eval.reason_path);
                        break;
                    }
                }
                result
            }
            Op::Not { operand } => {
                let eval = operand.eval(status_code, headers, body, gas)?;
                EvalResult::child_idx(0, !eval.result, eval.reason_path)
            }
            Op::StatusCodeCheck { value, operator } => {
                gas.consume(1)?;
                let value = match operator {
                    Comparison::Always => true,
                    Comparison::Never => false,
                    Comparison::LessThan => status_code < *value,
                    Comparison::GreaterThan => status_code > *value,
                    Comparison::Equal => *value == status_code,
                    Comparison::NotEqual => *value != status_code,
                };
                EvalResult::leaf(value)
            }
            Op::HeaderCheck {
                key_op,
                key_operand,
                value_op,
                value_operand,
            } => {
                // Find any header that passes key.  Then, see if it's value passes value.
                let mut result = EvalResult::leaf(false);

                for (k, v) in headers {
                    if key_operand.eval(key_op, k.as_str(), gas)? {
                        if let Ok(value_str) = v.to_str() {
                            if value_operand.eval(value_op, value_str, gas)? {
                                result = EvalResult::leaf(true);
                                break;
                            }
                        }
                    }
                }

                result
            }
            Op::JsonPath { value } => {
                let json: serde_json::Value =
                    serde_json::from_slice(body).map_err(|e| RuntimeError::InvalidBodyJson {
                        body: e.to_string(),
                    })?;
                let result = js_path_process(value, &json, gas.borrow_mut())?;
                EvalResult::leaf(!result.is_empty())
            }
        };

        Ok(result)
    }
}

pub fn compile(assertion: &super::Assertion) -> Result<Assertion, CompilationError> {
    Ok(Assertion {
        root: compile_op(&assertion.root)?,
    })
}

fn compile_op(op: &super::Op) -> Result<Op, CompilationError> {
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
        super::Op::HeaderCheck {
            key_op,
            key_operand,
            value_op,
            value_operand,
        } => Op::HeaderCheck {
            key_op: key_op.into(),
            value_op: value_op.into(),
            key_operand: key_operand.try_into()?,
            value_operand: value_operand.try_into()?,
        },
        super::Op::JsonPath { value } => Op::JsonPath {
            value: parse_json_path(value, GLOB_COMPLEXITY_LIMIT)?,
        },
    };

    Ok(op)
}

impl From<JsonPathError> for RuntimeError {
    fn from(value: JsonPathError) -> Self {
        match value {
            JsonPathError::TookTooLong => RuntimeError::TookTooLong,
            rest => RuntimeError::InvalidJsonPath {
                msg: rest.to_string(),
            },
        }
    }
}

impl From<JsonPathError> for CompilationError {
    fn from(value: JsonPathError) -> Self {
        match value {
            JsonPathError::PestError(error) => {
                let pos = match error.location {
                    pest::error::InputLocation::Pos(pos) => pos,
                    pest::error::InputLocation::Span((start, _)) => start,
                };
                CompilationError::JSONPathParser {
                    msg: error.to_string(),
                    path: error.line().to_owned(),
                    pos,
                }
            }
            JsonPathError::InvalidGlob(glob, reason) => {
                CompilationError::InvalidGlob { glob, msg: reason }
            }
            rest => CompilationError::InvalidJsonPath {
                msg: rest.to_string(),
            },
        }
    }
}

fn visit_children(children: &[super::Op]) -> Result<Vec<Op>, CompilationError> {
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
        compiled::{compile, extract_failure_data, CompilationError, EvalPath},
        Assertion, Comparison, GlobPattern, HeaderOperand, Op,
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
                        operator: Comparison::Equals,
                    },
                    Op::StatusCodeCheck {
                        value: 400,
                        operator: Comparison::NotEqual,
                    },
                ],
            },
        };

        let assert = compile(&assert).unwrap();
        assert!(assert.eval(200, &HeaderMap::new(), b"").unwrap().result);
        assert!(!assert.eval(105, &HeaderMap::new(), b"").unwrap().result);
        assert!(!assert.eval(95, &HeaderMap::new(), b"").unwrap().result);
        assert!(!assert.eval(299, &HeaderMap::new(), b"").unwrap().result);
        assert!(!assert.eval(305, &HeaderMap::new(), b"").unwrap().result);
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
                        operator: Comparison::Equals,
                    },
                ],
            },
        };
        let assert = compile(&assert).unwrap();
        assert!(assert.eval(200, &HeaderMap::new(), b"").unwrap().result);
        assert!(!assert.eval(105, &HeaderMap::new(), b"").unwrap().result);
        assert!(assert.eval(95, &HeaderMap::new(), b"").unwrap().result);
        assert!(!assert.eval(299, &HeaderMap::new(), b"").unwrap().result);
        assert!(assert.eval(305, &HeaderMap::new(), b"").unwrap().result);
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
                                operator: Comparison::Equals,
                            },
                            Op::StatusCodeCheck {
                                value: 202,
                                operator: Comparison::Equals,
                            },
                        ],
                    },
                ],
            },
        };

        let assert = compile(&assert).unwrap();
        assert!(!assert.eval(0, &HeaderMap::new(), b"").unwrap().result);
        assert!(assert.eval(15, &HeaderMap::new(), b"").unwrap().result);
        assert!(!assert.eval(100, &HeaderMap::new(), b"").unwrap().result);
        assert!(assert.eval(200, &HeaderMap::new(), b"").unwrap().result);
        assert!(assert.eval(201, &HeaderMap::new(), b"").unwrap().result);
        assert!(assert.eval(202, &HeaderMap::new(), b"").unwrap().result);
        assert!(!assert.eval(203, &HeaderMap::new(), b"").unwrap().result);
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
            key_op: Comparison::Equals,
            key_operand: HeaderOperand::Glob {
                pattern: GlobPattern {
                    value: "*-good*".to_owned(),
                },
            },
            value_op: Comparison::Always,
            value_operand: HeaderOperand::None,
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
        assert!(matches!(result, super::RuntimeError::TookTooLong));
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

        assert!(assert.eval(200, &hmap, body.as_bytes()).unwrap().result);

        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?length(@.prop1) > 10]".to_owned(),
            },
        };

        let assert = compile(&assert).unwrap();

        assert!(!assert.eval(200, &hmap, body.as_bytes()).unwrap().result);

        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?glob(@.prop1, \"a[a-z]*\")]".to_owned(),
            },
        };

        let assert = compile(&assert).unwrap();
        assert!(assert.eval(200, &hmap, body.as_bytes()).unwrap().result);

        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?glob(@.prop1, \"A[A-Z]*\")]".to_owned(),
            },
        };

        let assert = compile(&assert).unwrap();

        assert!(!assert.eval(200, &hmap, body.as_bytes()).unwrap().result);
    }

    #[test]
    fn test_header_regex() {
        let mut hmap = HeaderMap::new();
        hmap.append("x-header-good", HeaderValue::from_static("0"));
        hmap.append("x-header-good-a", HeaderValue::from_static("1"));
        hmap.append("x-header-good-1", HeaderValue::from_static("2"));

        let assert = Assertion {
            root: Op::HeaderCheck {
                key_op: Comparison::Equals,
                key_operand: HeaderOperand::Glob {
                    pattern: GlobPattern {
                        value: r"*-good-[0-9]".into(),
                    },
                },
                value_op: Comparison::Always,
                value_operand: HeaderOperand::None,
            },
        };
        let assert = compile(&assert).unwrap();
        assert!(assert.eval(200, &hmap, b"").unwrap().result);

        let assert = Assertion {
            root: Op::Or {
                children: vec![Op::HeaderCheck {
                    key_op: Comparison::Equals,
                    key_operand: HeaderOperand::Glob {
                        pattern: GlobPattern {
                            value: r"*-good-[0-9]".into(),
                        },
                    },
                    value_op: Comparison::Equals,
                    value_operand: HeaderOperand::Literal { value: "2".into() },
                }],
            },
        };
        let assert = compile(&assert).unwrap();
        assert!(assert.eval(200, &hmap, b"").unwrap().result);

        let assert = Assertion {
            root: Op::HeaderCheck {
                key_op: Comparison::Equals,
                key_operand: HeaderOperand::Glob {
                    pattern: GlobPattern {
                        value: r"*-good-[0-9]".into(),
                    },
                },

                value_op: Comparison::Equals,
                value_operand: HeaderOperand::Glob {
                    pattern: GlobPattern {
                        value: r"[0-9][0-9]".into(),
                    },
                },
            },
        };
        let assert = compile(&assert).unwrap();
        assert!(!assert.eval(200, &hmap, b"").unwrap().result);
    }

    #[test]
    fn test_broken_jsonpath_glob() {
        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?glob(@.prop1, \"'a']\")]".to_owned(),
            },
        };

        let assert = compile(&assert).unwrap_err();
        assert!(matches!(
            assert,
            CompilationError::InvalidGlob { msg: _, glob: _ }
        ));
    }

    #[test]
    fn test_broken_header_glob() {
        let assert = Assertion {
            root: Op::HeaderCheck {
                key_op: Comparison::Equals,
                key_operand: HeaderOperand::Glob {
                    pattern: GlobPattern {
                        value: "'a']".into(),
                    },
                },
                value_op: Comparison::Always,
                value_operand: HeaderOperand::None,
            },
        };

        let assert = compile(&assert).unwrap_err();
        assert!(matches!(
            assert,
            CompilationError::InvalidGlob { msg: _, glob: _ }
        ));
    }
    #[test]
    fn test_failing_path_or() {
        let mut hmap = HeaderMap::new();
        hmap.append("4", HeaderValue::from_static("*"));
        hmap.append("2", HeaderValue::from_static("*"));
        hmap.append("5", HeaderValue::from_static("*"));
        let assert = Assertion {
            root: Op::Not {
                operand: Op::Or {
                    children: vec![
                        Op::HeaderCheck {
                            key_op: Comparison::Equals,
                            key_operand: HeaderOperand::Literal { value: "1".into() },
                            value_op: Comparison::Always,
                            value_operand: HeaderOperand::None,
                        },
                        Op::HeaderCheck {
                            key_op: Comparison::Equals,
                            key_operand: HeaderOperand::Literal { value: "2".into() },
                            value_op: Comparison::Always,
                            value_operand: HeaderOperand::None,
                        },
                        Op::HeaderCheck {
                            key_op: Comparison::Equals,
                            key_operand: HeaderOperand::Literal { value: "3".into() },
                            value_op: Comparison::Always,
                            value_operand: HeaderOperand::None,
                        },
                    ],
                }
                .into(),
            },
        };
        let c_assert = compile(&assert).unwrap();
        let eval = c_assert.eval(200, &hmap, b"").unwrap();
        assert!(!eval.result);
        assert_eq!(
            eval.reason_path,
            EvalPath::ChildIndex {
                index: 0,
                child: EvalPath::ChildIndex {
                    index: 1,
                    child: EvalPath::Leaf.into()
                }
                .into()
            }
        );

        let extracted_failure = extract_failure_data(&eval.reason_path, &assert.root);
        match extracted_failure {
            Op::Not { operand } => match *operand {
                Op::Or { children } => assert_eq!(
                    children[0],
                    Op::HeaderCheck {
                        key_op: Comparison::Equals,
                        key_operand: HeaderOperand::Literal { value: "2".into() },
                        value_op: Comparison::Always,
                        value_operand: HeaderOperand::None,
                    }
                ),
                _ => assert!(false),
            },
            _ => assert!(false),
        }
    }

    #[test]
    fn test_failing_path_and() {
        let mut hmap = HeaderMap::new();
        hmap.append("1", HeaderValue::from_static("*"));
        hmap.append("2", HeaderValue::from_static("*"));
        hmap.append("3", HeaderValue::from_static("*"));
        let assert = Assertion {
            root: Op::And {
                children: vec![
                    Op::HeaderCheck {
                        key_op: Comparison::Equals,
                        key_operand: HeaderOperand::Literal { value: "1".into() },
                        value_op: Comparison::Always,
                        value_operand: HeaderOperand::None,
                    },
                    Op::HeaderCheck {
                        key_op: Comparison::Equals,
                        key_operand: HeaderOperand::Literal { value: "2".into() },
                        value_op: Comparison::Always,
                        value_operand: HeaderOperand::None,
                    },
                    Op::HeaderCheck {
                        key_op: Comparison::Equals,
                        key_operand: HeaderOperand::Literal { value: "4".into() },
                        value_op: Comparison::Always,
                        value_operand: HeaderOperand::None,
                    },
                ],
            },
        };
        let c_assert = compile(&assert).unwrap();
        let eval = c_assert.eval(200, &hmap, b"").unwrap();
        assert!(!eval.result);
        assert_eq!(
            eval.reason_path,
            EvalPath::ChildIndex {
                index: 2,
                child: EvalPath::Leaf.into()
            }
        );

        let extracted_failure = extract_failure_data(&eval.reason_path, &assert.root);
        match extracted_failure {
            Op::And { children } => assert_eq!(
                children[0],
                Op::HeaderCheck {
                    key_op: Comparison::Equals,
                    key_operand: HeaderOperand::Literal { value: "4".into() },
                    value_op: Comparison::Always,
                    value_operand: HeaderOperand::None,
                },
            ),
            _ => assert!(false),
        }
    }

    #[test]
    fn test_failing_path_nesting() {
        let mut hmap = HeaderMap::new();
        hmap.append("1", HeaderValue::from_static("*"));
        hmap.append("2", HeaderValue::from_static("*"));
        hmap.append("3", HeaderValue::from_static("*"));
        let assert = Assertion {
            root: Op::Not {
                operand: Op::Or {
                    children: vec![
                        Op::And {
                            children: vec![
                                Op::HeaderCheck {
                                    key_op: Comparison::Equals,
                                    key_operand: HeaderOperand::Literal { value: "1".into() },
                                    value_op: Comparison::Always,
                                    value_operand: HeaderOperand::None,
                                },
                                Op::HeaderCheck {
                                    key_op: Comparison::NotEqual,
                                    key_operand: HeaderOperand::Literal {
                                        value: "123".into(),
                                    },
                                    value_op: Comparison::Always,
                                    value_operand: HeaderOperand::None,
                                },
                            ],
                        },
                        Op::Or {
                            children: vec![
                                Op::Not {
                                    operand: Op::StatusCodeCheck {
                                        value: 200,
                                        operator: Comparison::Equals,
                                    }
                                    .into(),
                                },
                                Op::StatusCodeCheck {
                                    value: 201,
                                    operator: Comparison::Equals,
                                },
                            ],
                        },
                    ],
                }
                .into(),
            },
        };
        let c_assert = compile(&assert).unwrap();
        let eval = c_assert.eval(200, &hmap, b"").unwrap();
        assert!(!eval.result);
        assert_eq!(
            eval.reason_path,
            EvalPath::ChildIndex {
                index: 0,
                child: EvalPath::ChildIndex {
                    index: 0,
                    child: EvalPath::AllChildren.into()
                }
                .into()
            }
        );

        let extracted_failure = extract_failure_data(&eval.reason_path, &assert.root);
        match extracted_failure {
            Op::Not { operand } => match *operand {
                Op::Or { children } => match &children[0] {
                    Op::And { children } => assert_eq!(
                        *children,
                        vec![
                            Op::HeaderCheck {
                                key_op: Comparison::Equals,
                                key_operand: HeaderOperand::Literal { value: "1".into() },
                                value_op: Comparison::Always,
                                value_operand: HeaderOperand::None,
                            },
                            Op::HeaderCheck {
                                key_op: Comparison::NotEqual,
                                key_operand: HeaderOperand::Literal {
                                    value: "123".into(),
                                },
                                value_op: Comparison::Always,
                                value_operand: HeaderOperand::None,
                            },
                        ],
                    ),
                    _ => assert!(false),
                },
                _ => assert!(false),
            },
            _ => assert!(false),
        }
    }
}
